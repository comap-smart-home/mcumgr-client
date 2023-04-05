// Copyright © 2023 Vouch.io LLC

use anyhow::{bail, Error, Result};
use log::debug;
use log::info;
use serde_cbor;
use serde_json;
use sha2::{Digest, Sha256};
use std::fs::read;
use std::path::PathBuf;

use crate::cli::*;
use crate::nmp_hdr::*;
use crate::transfer::encode_request;
use crate::transfer::next_seq_id;
use crate::transfer::transceive;

pub fn list(cli: &Cli) -> Result<(), Error> {
    info!("send image list request");

    // send request
    let body: Vec<u8> =
        serde_cbor::to_vec(&std::collections::BTreeMap::<String, String>::new()).unwrap();
    let (data, request_header) = encode_request(
        cli,
        NmpOp::Read,
        NmpGroup::Image,
        NmpIdImage::State,
        &body,
        next_seq_id(),
    )?;
    let (response_header, response_body) = transceive(cli, data)?;

    // verify sequence id
    if response_header.seq != request_header.seq {
        bail!("wrong sequence number");
    }

    // verify response
    if response_header.op != NmpOp::ReadRsp || response_header.group != NmpGroup::Image {
        bail!("wrong response types");
    }

    // print body
    info!(
        "response: {}",
        serde_json::to_string_pretty(&response_body)?
    );

    Ok(())
}

pub fn upload(cli: &Cli, filename: &PathBuf) -> Result<(), Error> {
    info!("upload file: {}", filename.to_string_lossy());

    // load file
    let data = read(filename)?;
    info!("{} bytes to transfer", data.len());

    // transfer in blocks
    let mut off: usize = 0;
    loop {
        let off_start = off;
        let mut try_length = cli.mtu;
        debug!("try_length: {}", try_length);
        let seq_id = next_seq_id();
        loop {
            // create image upload request
            let image_num = cli.slot;
            if off + try_length > data.len() {
                try_length = data.len() - off;
            }
            let chunk = data[off..off + try_length].to_vec();
            let len = data.len() as u32;
            let req = if off == 0 {
                ImageUploadReq {
                    image_num,
                    off: off as u32,
                    len: Some(len),
                    data_sha: Some(Sha256::digest(&data).to_vec()),
                    upgrade: None,
                    data: chunk,
                }
            } else {
                ImageUploadReq {
                    image_num,
                    off: off as u32,
                    len: None,
                    data_sha: None,
                    upgrade: None,
                    data: chunk,
                }
            };
            debug!("req: {:?}", req);

            // convert to bytes with CBOR
            let body = serde_cbor::to_vec(&req)?;
            let (chunk, request_header) = encode_request(
                cli,
                NmpOp::Write,
                NmpGroup::Image,
                NmpIdImage::Upload,
                &body,
                seq_id,
            )?;

            // test if too long
            if chunk.len() > cli.mtu {
                let reduce = chunk.len() - cli.mtu;
                if reduce > try_length {
                    bail!("MTU too small");
                }

                // number of bytes to reduce is base64 encoded, calculate back the number of bytes
                // and then reduce a bit more for base64 filling and rounding
                try_length -= reduce * 3 / 4 + 3;
                debug!("new try_length: {}", try_length);
                continue;
            }

            // send request
            let (response_header, response_body) = transceive(cli, chunk)?;

            // verify sequence id
            if response_header.seq != request_header.seq {
                bail!("wrong sequence number");
            }

            // verify response
            if response_header.op != NmpOp::WriteRsp || response_header.group != NmpGroup::Image {
                bail!("wrong response types");
            }

            // verify result code and update offset
            debug!(
                "response_body: {}",
                serde_json::to_string_pretty(&response_body)?
            );
            if let serde_cbor::Value::Map(object) = response_body {
                for (key, val) in object.iter() {
                    match key {
                        serde_cbor::Value::Text(rc_key) if rc_key == "rc" => {
                            if let serde_cbor::Value::Integer(rc) = val {
                                if *rc != 0 {
                                    bail!("rc = {}", rc);
                                }
                            }
                        }
                        serde_cbor::Value::Text(off_key) if off_key == "off" => {
                            if let serde_cbor::Value::Integer(off_val) = val {
                                off = *off_val as usize;
                            }
                        }
                        _ => (),
                    }
                }
            }

            break;
        }

        // next chunk, next off should have been sent from the device
        if off_start == off {
            bail!("wrong offset received");
        }
        info!("{}% uploaded", 100 * off / data.len());
        if off == data.len() {
            break;
        }
    }
    info!("upload complete");
    Ok(())
}

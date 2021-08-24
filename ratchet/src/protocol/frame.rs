// Copyright 2015-2021 SWIM.AI inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::errors::{Error, ProtocolError};
use crate::framed::CodecFlags;
use crate::protocol::{HeaderFlags, OpCode};
use either::Either;
use std::convert::TryFrom;
use std::mem::size_of;

const U16_MAX: usize = u16::MAX as usize;

#[derive(Debug, Clone, PartialEq)]
pub struct FrameHeader {
    pub opcode: OpCode,
    pub flags: HeaderFlags,
    pub mask: Option<u32>,
}

macro_rules! try_parse_int {
    ($source:ident, $offset:ident, $source_length:ident, $into:ty) => {{
        const WIDTH: usize = size_of::<$into>();
        if $source_length < WIDTH + $offset {
            return Ok(Either::Right($offset + WIDTH - $source_length));
        }

        match <[u8; WIDTH]>::try_from(&$source[$offset..$offset + WIDTH]) {
            Ok(len) => {
                let len = <$into>::from_be_bytes(len);
                $offset += WIDTH;
                len
            }
            Err(_) => return Ok(Either::Right($offset + WIDTH - $source_length)),
        }
    }};
}

impl FrameHeader {
    pub fn read_from(
        source: &[u8],
        codec_flags: &CodecFlags,
        max_size: usize,
    ) -> Result<Either<(FrameHeader, usize, usize), usize>, Error> {
        let source_length = source.len();
        if source_length < 2 {
            return Ok(Either::Right(2 - source_length));
        }

        let server = codec_flags.contains(CodecFlags::ROLE);

        let first = source[0];
        let received_flags = HeaderFlags::from_bits_truncate(first);
        let opcode = OpCode::try_from(first & 0xF)?;

        if opcode.is_control() && !received_flags.is_fin() {
            // rfc6455 § 5.4: Control frames themselves MUST NOT be fragmented
            return Err(ProtocolError::FragmentedControl.into());
        }

        if (received_flags.bits() & !codec_flags.bits() & 0x70) != 0 {
            // Peer set a RSV bit high that hasn't been negotiated
            return Err(ProtocolError::UnknownExtension.into());
        }

        let second = source[1];
        let masked = second & 0x80 != 0;

        if !masked && server {
            // rfc6455 § 6.1: Client must send masked data
            return Err(ProtocolError::UnmaskedFrame.into());
        } else if masked && !server {
            // rfc6455 § 6.2: Server must remove masking
            return Err(ProtocolError::MaskedFrame.into());
        }

        let payload_length = second & 0x7F;
        let mut offset = 2;

        let length: usize = if payload_length == 126 {
            try_parse_int!(source, offset, source_length, u16) as usize
        } else if payload_length == 127 {
            try_parse_int!(source, offset, source_length, u64) as usize
        } else {
            usize::from(payload_length)
        };

        if length > max_size {
            return Err(ProtocolError::FrameOverflow.into());
        }

        let mask = if masked {
            Some(try_parse_int!(source, offset, source_length, u32))
        } else {
            None
        };

        Ok(Either::Left((
            (FrameHeader {
                opcode,
                flags: received_flags,
                mask,
            }),
            offset,
            length,
        )))
    }
}

// todo speed up with an XOR lookup table
pub fn apply_mask(mask: u32, bytes: &mut [u8]) {
    let mask: [u8; 4] = mask.to_be_bytes();

    for i in 0..bytes.len() {
        bytes[i] ^= mask[i & 0x3]
    }
}

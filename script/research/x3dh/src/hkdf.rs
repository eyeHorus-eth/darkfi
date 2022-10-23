//! https://tools.ietf.org/html/rfc5869
use core::fmt;
use sha2::{
    digest::{crypto_common::BlockSizeUser, typenum::Unsigned, Output, OutputSizeUser, Update},
    Digest,
};

use super::hmac::Hmac;

#[derive(Copy, Clone, Debug)]
pub struct InvalidPrkLength;

impl fmt::Display for InvalidPrkLength {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        f.write_str("invalid pseudorandom key length, too short")
    }
}

// Structure for InvalidLength, used for output error handling.
#[derive(Copy, Clone, Debug)]
pub struct InvalidLength;

impl fmt::Display for InvalidLength {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        f.write_str("invalid number of blocks, too large output")
    }
}

#[derive(Clone)]
pub struct HkdfExtract<H: Digest + BlockSizeUser + Clone> {
    hmac: Hmac<H>,
}

impl<H: Digest + BlockSizeUser + Clone> HkdfExtract<H> {
    pub fn new(salt: &[u8]) -> Self {
        Self { hmac: Hmac::<H>::new_from_slice(salt) }
    }

    pub fn input_ikm(&mut self, ikm: &[u8]) {
        self.hmac.update(ikm);
    }

    pub fn finalize(self) -> (Output<H>, Hkdf<H>) {
        let prk = self.hmac.finalize();
        let hkdf = Hkdf::from_prk(&prk).expect("PRK size is correct");
        (prk, hkdf)
    }
}

#[derive(Clone)]
pub struct Hkdf<H: Digest + BlockSizeUser + Clone> {
    hmac: Hmac<H>,
}

impl<H: Digest + BlockSizeUser + Clone> Hkdf<H> {
    pub fn new(salt: &[u8], ikm: &[u8]) -> Self {
        let (_, hkdf) = Self::extract(salt, ikm);
        hkdf
    }

    pub fn extract(salt: &[u8], ikm: &[u8]) -> (Output<H>, Self) {
        let mut extract_ctx = HkdfExtract::new(salt);
        extract_ctx.input_ikm(ikm);
        extract_ctx.finalize()
    }

    pub fn from_prk(prk: &[u8]) -> Result<Self, InvalidPrkLength> {
        if prk.len() < <H as OutputSizeUser>::OutputSize::to_usize() {
            return Err(InvalidPrkLength)
        }

        Ok(Self { hmac: Hmac::<H>::new_from_slice(prk) })
    }

    pub fn expand(&self, info: &[u8], okm: &mut [u8]) -> Result<(), InvalidLength> {
        self.expand_multi_info(&[info], okm)
    }

    pub fn expand_multi_info(&self, infos: &[&[u8]], okm: &mut [u8]) -> Result<(), InvalidLength> {
        let mut prev: Option<Output<H>> = None;

        let chunk_len = <H as OutputSizeUser>::OutputSize::USIZE;
        if okm.len() > chunk_len * 255 {
            return Err(InvalidLength)
        }

        for (block_n, block) in okm.chunks_mut(chunk_len).enumerate() {
            let mut hmac = self.hmac.clone();

            if let Some(ref prev) = prev {
                hmac.update(prev);
            }

            // Feed in the info components in sequence. This is equivalent
            // to feeding in the concatenation of all the info components.
            for info in infos {
                hmac.update(info);
            }

            hmac.update(&[block_n as u8 + 1]);

            let output = hmac.finalize();
            let block_len = block.len();
            block.copy_from_slice(&output[..block_len]);

            prev = Some(output);
        }

        Ok(())
    }
}
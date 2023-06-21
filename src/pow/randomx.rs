pub use randomx_rs::RandomXFlag;
use randomx_rs::{RandomXCache, RandomXDataset, RandomXError, RandomXVM};
use rayon::prelude::{IntoParallelIterator, ParallelIterator};
use thread_local::ThreadLocal;

use super::Error;

const RANDOMX_CACHE_KEY: &[u8] = b"spacemesh-randomx-cache-key";

impl From<randomx_rs::RandomXError> for Error {
    fn from(e: randomx_rs::RandomXError) -> Self {
        Error::Internal(Box::new(e))
    }
}

pub struct PoW {
    cache: Option<RandomXCache>,
    dataset: Option<RandomXDataset>,
    flags: RandomXFlag,
    vms: ThreadLocal<RandomXVM>,
}

impl PoW {
    pub fn new(flags: RandomXFlag) -> Result<PoW, Error> {
        let cache = RandomXCache::new(flags, RANDOMX_CACHE_KEY)?;
        let (cache, dataset) = if flags.contains(RandomXFlag::FLAG_FULL_MEM) {
            (None, Some(RandomXDataset::new(flags, cache, 0)?))
        } else {
            (Some(cache), None)
        };

        Ok(Self {
            cache,
            dataset,
            flags,
            vms: ThreadLocal::new(),
        })
    }

    fn get_vm(&self) -> Result<&RandomXVM, RandomXError> {
        self.vms
            .get_or_try(|| RandomXVM::new(self.flags, self.cache.clone(), self.dataset.clone()))
    }

    pub fn prove(
        &self,
        nonce_group: u8,
        challenge: &[u8; 8],
        difficulty: &[u8; 32],
    ) -> Result<u64, Error> {
        let pow_input = [[0u8; 7].as_slice(), [nonce_group].as_slice(), challenge].concat();

        let (pow_nonce, _) = (0..2u64.pow(56))
            .into_par_iter()
            .map_init(
                || -> Result<_, Error> { Ok((self.get_vm()?, pow_input.clone())) },
                |state, pow_nonce| {
                    if let Ok((vm, pow_input)) = state {
                        pow_input[0..7].copy_from_slice(&pow_nonce.to_le_bytes()[0..7]);
                        let hash = vm.calculate_hash(pow_input.as_slice()).ok()?;
                        Some((pow_nonce, hash))
                    } else {
                        None
                    }
                },
            )
            .filter_map(|res| res)
            .find_any(|(_, hash)| hash.as_slice() < difficulty)
            .ok_or(Error::PoWNotFound)?;

        Ok(pow_nonce)
    }

    pub fn verify(
        &self,
        pow: u64,
        nonce_group: u8,
        challenge: &[u8; 8],
        difficulty: &[u8; 32],
    ) -> Result<(), Error> {
        log::info!("Verifying RandomX pow for nonce: {pow}, nonce_group: {nonce_group}, challenge: {challenge:X?}, difficulty: {difficulty:X?}");
        let pow_input = [
            &pow.to_le_bytes()[0..7],
            [nonce_group].as_slice(),
            challenge,
        ]
        .concat();
        let vm = self.get_vm()?;
        let hash = vm.calculate_hash(pow_input.as_slice())?;

        if hash.as_slice() >= difficulty {
            return Err(Error::InvalidPoW);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pow() {
        let nonce = 7;
        let challenge = b"hello!!!";
        let difficulty = &[
            0x0f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff,
        ];
        let prover = PoW::new(dbg!(RandomXFlag::get_recommended_flags())).unwrap();
        let pow = prover.prove(nonce, challenge, difficulty).unwrap();
        prover.verify(pow, nonce, challenge, difficulty).unwrap();
    }

    #[test]
    fn test_pow_no_jit() {
        let nonce = 7;
        let challenge = b"hello!!!";
        let difficulty = &[
            0x0f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff,
        ];
        let flags =
            RandomXFlag::get_recommended_flags().symmetric_difference(RandomXFlag::FLAG_JIT);
        let prover = PoW::new(dbg!(flags)).unwrap();
        let pow = prover.prove(nonce, challenge, difficulty).unwrap();
        prover.verify(pow, nonce, challenge, difficulty).unwrap();
    }

    #[test]
    fn different_cache_key_gives_different_hash() {
        let input = b"hello world";
        let flags = RandomXFlag::get_recommended_flags();

        let cache = RandomXCache::new(flags, b"key0").unwrap();
        let vm = RandomXVM::new(flags, Some(cache), None).unwrap();
        let hash_0 = vm.calculate_hash(input).unwrap();

        let cache = RandomXCache::new(flags, b"key1").unwrap();
        let vm = RandomXVM::new(flags, Some(cache), None).unwrap();
        let hash_1 = vm.calculate_hash(input).unwrap();

        assert_ne!(hash_0, hash_1);
    }

    #[test]
    fn get_recommended_flags() {
        dbg!(RandomXFlag::get_recommended_flags());
    }
}

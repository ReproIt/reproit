//! Operating-system and external-system support.

mod hash;
mod scoped_env;

pub(crate) use hash::sha256_hex;
pub(crate) use scoped_env::ScopedEnv;

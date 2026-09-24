use deno_core::op2;

use crate::prelude::VERSION;

#[op2]
#[string]
pub fn get_lodestone_version() -> String {
    VERSION.with(|v| v.to_string())
}

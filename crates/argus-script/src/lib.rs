//! Scripting (Phase 2, minimal slice).
//!
//! Runs page JavaScript through the first-party **kataan** engine. This is the
//! beginning of `docs/subsystems/scripting.md`, but a deliberately small one:
//! kataan executes the language (computation, `console`), and we surface its
//! console output — but there are **no DOM bindings yet**. A script can compute
//! and log, but `document`/`window` are undefined, because exposing host objects
//! requires kataan's embedding API (native functions, exotic objects, GC handles),
//! which is tracked in `docs/upstream/kataan.md` and gates the rest of Phase 2.
//!
//! Caveat: `kataan::nbvm::execute` needs kataan's `std` feature, which links its
//! OS-reaching host (rsurl/fs/crypto). In the sandboxed content process those are
//! linked but unreachable (the sandbox denies the syscalls); the clean split —
//! kataan core + Argus-supplied host bindings — also awaits the kataan embedding
//! API. Until then this meaningfully runs only pure, side-effect-free scripts.

/// The result of running one script.
#[derive(Clone, Debug)]
pub struct ScriptResult {
    /// Captured `console` output.
    pub console: String,
    /// Display string of the program's final value.
    pub value: String,
}

/// Run a JavaScript source string, returning its console output and final value,
/// or an error message (parse or runtime error).
pub fn run_script(src: &str) -> Result<ScriptResult, String> {
    kataan::nbvm::execute(src).map(|(console, value)| ScriptResult { console, value })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_and_logs() {
        let r = run_script("console.log('hello'); 1 + 2 * 3").unwrap();
        assert_eq!(r.console, "hello\n");
        assert_eq!(r.value, "7");
    }

    #[test]
    fn functions_and_loops() {
        let r = run_script(
            "function fib(n){ return n < 2 ? n : fib(n-1) + fib(n-2); } console.log(fib(10));",
        )
        .unwrap();
        assert_eq!(r.console.trim(), "55");
    }
}

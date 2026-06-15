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

/// Run `source`, then — in the **same realm**, after kataan's event loop has
/// drained (promise microtasks, `setTimeout` macrotasks, async tails) — evaluate
/// `followup` and return its string value plus the combined console output.
///
/// This lets a host capture a result that asynchronous code populates: e.g. run
/// page scripts that record DOM mutations into a global array, then read
/// `JSON.stringify(thatArray)` once everything (including `.then`/`setTimeout`
/// callbacks) has run. Returns `Err` if compilation fails (the bytecode path
/// doesn't support a construct) so the caller can fall back.
pub fn run_with_followup(source: &str, followup: &str) -> Result<(String, String), String> {
    use kataan::nbexec::Interp;
    use kataan::parser::Parser;

    // The tree-walker (not the bytecode path) supports the DOM shim's constructs
    // (Proxy, full globals) and persists realm state across `run` calls; each `run`
    // drains the event loop (promise microtasks + `setTimeout`) before returning.
    let prog = Parser::parse_program(source).map_err(|e| format!("parse: {e}"))?;
    let fprog = Parser::parse_program(followup).map_err(|e| format!("parse followup: {e}"))?;
    let mut interp = Interp::new_with_limits(kataan::limits::Limits::default());
    interp.run(&prog).map_err(|e| format!("run: {e:?}"))?;
    let val = interp.run(&fprog).map_err(|e| format!("run followup: {e:?}"))?;
    let output = interp.output().to_string();
    Ok((output, interp.display(val)))
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
    fn followup_reads_global_after_run() {
        let (_c, v) =
            run_with_followup("a = []; a.push(1); a.push(2);", "a.join('-')")
                .unwrap();
        assert_eq!(v, "1-2");
    }

    #[test]
    fn followup_works_with_proxy() {
        // The DOM shim relies on Proxy; confirm the bytecode path handles it.
        let src = "log = []; var p = new Proxy({}, { set: function(t,k,v){ log.push(k+'='+v); return true; } }); p.x = 1; p.y = 2;";
        let (_c, v) = run_with_followup(src, "log.join(',')").unwrap();
        assert_eq!(v, "x=1,y=2");
    }

    #[test]
    fn followup_captures_async_mutations() {
        // The key win: values written in a Promise.then / setTimeout callback are
        // visible to the followup, because it runs after the event loop drains.
        let src = "out = [];\
            Promise.resolve().then(function(){ out.push('microtask'); });\
            setTimeout(function(){ out.push('timer'); }, 0);\
            out.push('sync');";
        let (_c, v) = run_with_followup(src, "out.join(',')").unwrap();
        assert!(
            v.contains("sync") && v.contains("microtask") && v.contains("timer"),
            "got: {v}"
        );
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

//! Phase 2 DOM bindings via a JS-side shim + post-execution reconciliation.
//!
//! kataan has no host-callback API, but it supports enough JavaScript (ES6
//! `Proxy` get/set traps, `Object.defineProperty`, `JSON`, closures) to model
//! `document`/`window` *entirely in JS*: a prelude defines proxies whose traps
//! record DOM mutations into an array. We seed that prelude with the real DOM's
//! id'd elements, run prelude + page scripts through kataan once, then read the
//! recorded ops back and apply them to the real [`Document`] before layout.
//!
//! This is a pragmatic subset — no live reflow — but it makes a real chunk of the
//! DOM API actually change the rendered page, plus discrete `click` handlers (via
//! deterministic replay through [`apply_scripts_with_events`]), `setTimeout`/
//! `setInterval`/`requestAnimationFrame` callbacks (shim-queued, drained
//! earliest-delay-first, no wall clock; rAF gets a synthetic timestamp), **async
//! DOM mutations** — writes inside `Promise.then`/`async`-`await`
//! callbacks are reconciled too, because scripts run through
//! [`argus_script::run_with_followup`], which drains the engine's event loop
//! (promise microtasks + async tails) before the ops array is read back — and
//! `localStorage` (persisted across navigations within
//! the content process via [`apply_scripts_session`]) / `sessionStorage`
//! (per-page). The DOM API surface includes:
//! `document.getElementById` / `querySelector` (full CSS selector engine) /
//! `createElement` / `body` / `write`, and on elements `textContent`/`innerText`,
//! `innerHTML`, `className`, `setAttribute`/`getAttribute`, `style.<camelCase>`,
//! `classList`, scoped `querySelector`, and `appendChild`/`append`/`remove`.

use argus_dom::{Attribute, Document, NodeData, NodeId, QualName};

/// The JS prelude defining `document`, `window`, and proxy element handles.
/// `__SEED__` is replaced with a JSON object of `{ id: { textContent, ... } }`.
const PRELUDE: &str = r#"
var __argus_ops = [];
var __seed = __SEED__;
var __argus_state = {};      // in-JS overlay so reads reflect prior writes this run
var __argus_listeners = {};  // event listeners keyed by target, for replay dispatch
function __sk(tgt) { return tgt.kind + "" + tgt.val; }
function __reg(tgt, type, fn) {
  var key = __sk(tgt);
  (__argus_listeners[key] = __argus_listeners[key] || []).push({type: "" + type, fn: fn});
}
// Read the current value of a property/attribute (overlay first, then seed).
function __read(tgt, seed, k) {
  var sk = __sk(tgt);
  if (__argus_state[sk] && __argus_state[sk][k] != null) return __argus_state[sk][k];
  if (seed && seed[k] != null) return seed[k];
  return null;
}
// `tgt` is {kind:"id"|"sel", val:"..."}. Seeded reads are only available for ids.
function __argus_el(tgt) {
  var seed = (tgt.kind === "id") ? __seed[tgt.val] : null;
  return new Proxy({}, {
    set: function(t, k, v) {
      // `on<event> = fn` registers an event listener; everything else is a prop.
      if (k.indexOf("on") === 0 && typeof v === "function") {
        __reg(tgt, k.substring(2), v);
        return true;
      }
      var sk = __sk(tgt);
      __argus_state[sk] = __argus_state[sk] || {};
      __argus_state[sk][k] = "" + v;
      __argus_ops.push({op: "prop", tgt: tgt, key: k, value: "" + v});
      return true;
    },
    get: function(t, k) {
      if (k === "style") {
        return new Proxy({}, {
          set: function(t2, k2, v2) {
            __argus_ops.push({op: "style", tgt: tgt, key: k2, value: "" + v2});
            return true;
          },
          get: function(t2, k2) { return ""; }
        });
      }
      if (k === "__tgt") return tgt;
      if (k === "classList") {
        var has = function(c) {
          var cur = __read(tgt, seed, "class");
          return cur != null && (" " + cur + " ").indexOf(" " + c + " ") >= 0;
        };
        return {
          add: function(c) { __argus_ops.push({op: "class", tgt: tgt, key: "add", value: "" + c}); },
          remove: function(c) { __argus_ops.push({op: "class", tgt: tgt, key: "remove", value: "" + c}); },
          toggle: function(c) { __argus_ops.push({op: "class", tgt: tgt, key: "toggle", value: "" + c}); },
          contains: has
        };
      }
      if (k === "addEventListener") {
        return function(type, fn) { __reg(tgt, type, fn); };
      }
      if (k === "setAttribute") {
        return function(name, val) {
          var sk = __sk(tgt);
          __argus_state[sk] = __argus_state[sk] || {};
          __argus_state[sk]["" + name] = "" + val;
          __argus_ops.push({op: "attr", tgt: tgt, key: "" + name, value: "" + val});
        };
      }
      if (k === "getAttribute") {
        return function(name) { var r = __read(tgt, seed, "" + name); return r == null ? null : r; };
      }
      if (k === "removeAttribute") {
        return function(name) {
          var sk = __sk(tgt);
          if (__argus_state[sk]) __argus_state[sk]["" + name] = null;
          __argus_ops.push({op: "removeattr", tgt: tgt, key: "" + name});
        };
      }
      if (k === "hasAttribute") {
        return function(name) { return __read(tgt, seed, "" + name) != null; };
      }
      if (k === "appendChild" || k === "append") {
        return function(child) {
          __argus_ops.push({op: "append", tgt: tgt, child: child ? child.__tgt : null});
          return child;
        };
      }
      if (k === "insertBefore") {
        return function(child, ref) {
          __argus_ops.push({op: "insertBefore", tgt: tgt,
            child: child ? child.__tgt : null, ref: ref ? ref.__tgt : null});
          return child;
        };
      }
      if (k === "remove") {
        return function() { __argus_ops.push({op: "remove", tgt: tgt}); };
      }
      if (k === "querySelector") {
        return function(sel) { return __argus_el({kind: "scoped", parent: tgt, val: "" + sel}); };
      }
      var r = __read(tgt, seed, k);
      return r == null ? "" : r;
    }
  });
}
// Fire registered handlers of `type` on element {kind,val} (for replay dispatch).
function __argus_dispatch(kind, val, type) {
  var ls = __argus_listeners[kind + "" + val];
  if (!ls) return;
  var ev = {type: type, target: __argus_el({kind: kind, val: val})};
  for (var i = 0; i < ls.length; i++) {
    if (ls[i].type === type) { ls[i].fn(ev); }
  }
}
var __newCount = 0;
var document = {
  getElementById: function(id) { return __argus_el({kind: "id", val: "" + id}); },
  querySelector: function(sel) { return __argus_el({kind: "sel", val: "" + sel}); },
  createElement: function(tag) {
    var nid = "n" + (__newCount++);
    __argus_ops.push({op: "create", nid: nid, tag: "" + tag});
    return __argus_el({kind: "new", val: nid});
  },
  write: function(s) { __argus_ops.push({op: "write", value: "" + s}); }
};
document.body = __argus_el({kind: "sel", val: "body"});
document.documentElement = __argus_el({kind: "sel", val: "html"});
var window = document.window = document;

// Timers: there is no wall clock in the synchronous reconciliation model, so
// scheduled callbacks are queued and drained (earliest delay first) after the
// script + event dispatches run. This makes deferred-init patterns work; it does
// not animate over real time. `setInterval` fires once (bounded), and the drain
// is capped to avoid infinite re-scheduling.
var __timers = [];
var __timerOrder = 0;
function setTimeout(fn, delay) {
  if (typeof fn !== "function") return 0;
  var id = ++__timerOrder;
  __timers.push({id: id, fn: fn, delay: (+delay) || 0});
  return id;
}
function setInterval(fn, delay) { return setTimeout(fn, delay); }
// requestAnimationFrame: no wall clock, so a frame callback is just a timer with
// a ~16ms delay; it's drained like the rest and handed a monotonic timestamp.
function requestAnimationFrame(fn) { return setTimeout(fn, 16); }
function cancelAnimationFrame(id) { clearTimeout(id); }
function clearTimeout(id) {
  for (var i = 0; i < __timers.length; i++) {
    if (__timers[i].id === id) { __timers.splice(i, 1); return; }
  }
}
function clearInterval(id) { clearTimeout(id); }
window.setTimeout = setTimeout; window.setInterval = setInterval;
window.clearTimeout = clearTimeout; window.clearInterval = clearInterval;
window.requestAnimationFrame = requestAnimationFrame;
window.cancelAnimationFrame = cancelAnimationFrame;
var __rafClock = 0;
function __argus_drain() {
  var iters = 0;
  while (__timers.length > 0 && iters < 1000) {
    var bi = 0;
    for (var i = 1; i < __timers.length; i++) {
      if (__timers[i].delay < __timers[bi].delay) bi = i;
    }
    var t = __timers.splice(bi, 1)[0];
    // Pass a synthetic, monotonically increasing timestamp; setTimeout callbacks
    // ignore the extra argument, rAF callbacks use it.
    __rafClock += 16;
    t.fn(__rafClock);
    iters++;
  }
}

// Storage. `localStorage` is seeded from the persistent store and records mutation
// ops, so writes survive across navigations within the session (reconciled in Rust,
// like the DOM). `sessionStorage` is in-execution only. Both are consistent within a
// run + replayed events.
var __storage_seed = __STORAGE__;
function __mkStorage(persist) {
  var data = {};
  for (var k in __storage_seed) {
    if (__storage_seed.hasOwnProperty(k)) data[k] = __storage_seed[k];
  }
  return {
    getItem: function(k) { var v = data["" + k]; return v == null ? null : v; },
    setItem: function(k, v) {
      data["" + k] = "" + v;
      if (persist) __argus_ops.push({op: "storage", key: "set", value: "" + k, value2: "" + v});
    },
    removeItem: function(k) {
      delete data["" + k];
      if (persist) __argus_ops.push({op: "storage", key: "remove", value: "" + k});
    },
    clear: function() {
      data = {};
      if (persist) __argus_ops.push({op: "storage", key: "clear"});
    },
    key: function(i) { var ks = Object.keys(data); return i < ks.length ? ks[i] : null; }
  };
}
var localStorage = __mkStorage(true);
var sessionStorage = __mkStorage(false);
window.localStorage = localStorage;
window.sessionStorage = sessionStorage;
"#;

/// One past interaction to replay: fire `event` on the element identified by
/// `kind` (`"id"`/`"sel"`) + `val`. Replaying the full history each run lets JS
/// state (and DOM read-backs via the overlay) accumulate deterministically.
#[derive(Clone, Debug)]
pub struct Interaction {
    pub kind: String,
    pub val: String,
    pub event: String,
}

/// Run a document's inline scripts and apply their DOM mutations in place.
/// Returns the console output (minus the internal ops line) for logging.
pub fn apply_scripts(doc: &mut Document) -> Option<String> {
    let mut storage = std::collections::HashMap::new();
    run_scripts(doc, &[], &mut storage)
}

/// Like [`apply_scripts`], but also replays `events` (deterministic event replay)
/// with a throwaway storage (no cross-call persistence).
pub fn apply_scripts_with_events(doc: &mut Document, events: &[Interaction]) -> Option<String> {
    let mut storage = std::collections::HashMap::new();
    run_scripts(doc, events, &mut storage)
}

/// The full session entry point: run scripts, replay `events`, and persist
/// `localStorage` writes into `storage` (seeded from it), so a long-lived caller
/// (the content process) keeps storage across navigations.
pub fn apply_scripts_session(
    doc: &mut Document,
    events: &[Interaction],
    storage: &mut std::collections::HashMap<String, String>,
) -> Option<String> {
    run_scripts(doc, events, storage)
}

fn run_scripts(
    doc: &mut Document,
    events: &[Interaction],
    storage: &mut std::collections::HashMap<String, String>,
) -> Option<String> {
    // A Content-Security-Policy meta can forbid inline scripts; honor it.
    if csp_blocks_inline_scripts(doc) {
        return Some("[CSP] blocked inline scripts\n".to_string());
    }
    let scripts = collect_scripts(doc);
    if scripts.is_empty() {
        return None;
    }

    let seed = seed_json(doc);
    let storage_seed = storage_json(storage);
    let mut src = PRELUDE
        .replace("__SEED__", &seed)
        .replace("__STORAGE__", &storage_seed);
    for s in &scripts {
        src.push('\n');
        src.push_str(s);
    }
    for e in events {
        src.push_str(&format!(
            "\n__argus_dispatch({}, {}, {});",
            json_string(&e.kind),
            json_string(&e.val),
            json_string(&e.event)
        ));
    }
    // Drain shim-scheduled timers (synchronous, delay-ordered). Native promise
    // microtasks and async tails are drained by the engine's own event loop
    // between the two phases of `run_with_followup`, so ops recorded inside
    // `.then`/`await`/async callbacks are visible to the followup read below.
    src.push_str("\n__argus_drain();");

    // Run the scripts, then — once the event loop has drained — read the recorded
    // ops as the followup's value. If the tree-walker rejects a construct, fall
    // back to the synchronous bytecode path so we never regress to running nothing.
    let (console, ops_json) =
        match argus_script::run_with_followup(&src, "JSON.stringify(__argus_ops)") {
            Ok(pair) => pair,
            Err(_) => run_sync_fallback(&src)?,
        };
    apply_ops(doc, &ops_json, storage);
    Some(console)
}

/// Synchronous fallback: run the shim through the bytecode VM and recover the ops
/// array from a sentinel console line. Used when the async tree-walker path errors
/// on some construct, so async-free pages still execute.
fn run_sync_fallback(src: &str) -> Option<(String, String)> {
    let mut s = src.to_string();
    s.push_str("\nconsole.log(\"\\u0001ARGUSOPS\" + JSON.stringify(__argus_ops));\n");
    let result = argus_script::run_script(&s).ok()?;
    let mut console = String::new();
    let mut ops = String::from("[]");
    for line in result.console.lines() {
        if let Some(json) = line
            .strip_prefix('\u{1}')
            .and_then(|l| l.strip_prefix("ARGUSOPS"))
        {
            ops = json.to_string();
        } else {
            console.push_str(line);
            console.push('\n');
        }
    }
    Some((console, ops))
}

/// Serialize the storage map to a JSON object for the prelude seed.
fn storage_json(storage: &std::collections::HashMap<String, String>) -> String {
    let entries: Vec<String> = storage
        .iter()
        .map(|(k, v)| format!("{}:{}", json_string(k), json_string(v)))
        .collect();
    format!("{{{}}}", entries.join(","))
}

/// Whether a `<meta http-equiv="Content-Security-Policy">` forbids inline scripts.
/// Inline scripts are blocked when the effective directive (`script-src`, else
/// `default-src`) is present and lacks `'unsafe-inline'` (and isn't `*`).
fn csp_blocks_inline_scripts(doc: &Document) -> bool {
    let Some(policy) = find_csp(doc) else {
        return false;
    };
    let directive =
        csp_directive(&policy, "script-src").or_else(|| csp_directive(&policy, "default-src"));
    match directive {
        None => false, // no script-src/default-src → scripts aren't restricted
        Some(tokens) => {
            let allows = tokens
                .split_whitespace()
                .any(|t| t == "'unsafe-inline'" || t == "*");
            !allows
        }
    }
}

/// The CSP policy string from a `<meta http-equiv>` tag, if present.
fn find_csp(doc: &Document) -> Option<String> {
    fn walk(doc: &Document, id: NodeId) -> Option<String> {
        if let NodeData::Element(e) = &doc.node(id).data {
            if e.name.is_html("meta")
                && e.attr("http-equiv")
                    .is_some_and(|v| v.eq_ignore_ascii_case("content-security-policy"))
            {
                return e.attr("content").map(|s| s.to_ascii_lowercase());
            }
        }
        for c in doc.children(id) {
            if let Some(p) = walk(doc, c) {
                return Some(p);
            }
        }
        None
    }
    walk(doc, doc.root())
}

/// Extract a named directive's value (the tokens after the name) from a policy.
fn csp_directive<'a>(policy: &'a str, name: &str) -> Option<&'a str> {
    policy.split(';').find_map(|d| {
        let d = d.trim();
        d.strip_prefix(name)
            .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
            .map(|rest| rest.trim())
    })
}

/// Collect inline (`src`-less) `<script>` text in document order.
fn collect_scripts(doc: &Document) -> Vec<String> {
    fn walk(doc: &Document, id: NodeId, out: &mut Vec<String>) {
        if let NodeData::Element(e) = &doc.node(id).data {
            if e.name.is_html("script") && e.attr("src").is_none() {
                let mut src = String::new();
                for c in doc.children(id) {
                    if let NodeData::Text(t) = &doc.node(c).data {
                        src.push_str(t);
                    }
                }
                if !src.trim().is_empty() {
                    out.push(src);
                }
            }
        }
        for c in doc.children(id) {
            walk(doc, c, out);
        }
    }
    let mut out = Vec::new();
    walk(doc, doc.root(), &mut out);
    out
}

/// Build the `__seed` JSON: each id'd element's text content and attributes,
/// so read-modify-write scripts see initial values.
fn seed_json(doc: &Document) -> String {
    let mut entries: Vec<String> = Vec::new();
    fn walk(doc: &Document, id: NodeId, entries: &mut Vec<String>) {
        if let NodeData::Element(e) = &doc.node(id).data {
            if let Some(eid) = e.attr("id") {
                let mut fields = vec![format!(
                    "\"textContent\":{}",
                    json_string(&text_content(doc, id))
                )];
                for a in &e.attrs {
                    fields.push(format!(
                        "{}:{}",
                        json_string(&a.name),
                        json_string(&a.value)
                    ));
                }
                entries.push(format!("{}:{{{}}}", json_string(eid), fields.join(",")));
            }
        }
        for c in doc.children(id) {
            walk(doc, c, entries);
        }
    }
    walk(doc, doc.root(), &mut entries);
    format!("{{{}}}", entries.join(","))
}

/// The concatenated text of an element's descendants.
fn text_content(doc: &Document, id: NodeId) -> String {
    let mut out = String::new();
    fn walk(doc: &Document, id: NodeId, out: &mut String) {
        match &doc.node(id).data {
            NodeData::Text(t) => out.push_str(t),
            _ => {
                for c in doc.children(id) {
                    walk(doc, c, out);
                }
            }
        }
    }
    walk(doc, id, &mut out);
    out
}

/// Apply the ops array (parsed from JSON) to the document.
fn apply_ops(
    doc: &mut Document,
    json: &str,
    storage: &mut std::collections::HashMap<String, String>,
) {
    use std::collections::HashMap;
    let Some(Json::Arr(ops)) = parse_json(json) else {
        return;
    };
    // Detached elements created by `document.createElement`, keyed by synthetic id.
    let mut created: HashMap<String, NodeId> = HashMap::new();

    for op in ops {
        let Json::Obj(fields) = op else { continue };
        let get =
            |k: &str| -> Option<&Json> { fields.iter().find(|(n, _)| n == k).map(|(_, v)| v) };
        let op_kind = get("op").and_then(Json::as_str).unwrap_or("");
        let value = get("value")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_string();
        let key = get("key").and_then(Json::as_str).unwrap_or("").to_string();

        if op_kind == "storage" {
            // localStorage mutation: key="set"/"remove"/"clear", value=storage key.
            match key.as_str() {
                "set" => {
                    let v2 = get("value2")
                        .and_then(Json::as_str)
                        .unwrap_or("")
                        .to_string();
                    storage.insert(value, v2);
                }
                "remove" => {
                    storage.remove(&value);
                }
                "clear" => storage.clear(),
                _ => {}
            }
            continue;
        }
        if op_kind == "create" {
            let nid = get("nid").and_then(Json::as_str).unwrap_or("");
            let tag = get("tag").and_then(Json::as_str).unwrap_or("div");
            let node = doc.create_element(QualName::html(tag.to_ascii_lowercase()), vec![]);
            if !nid.is_empty() {
                created.insert(nid.to_string(), node);
            }
            continue;
        }
        if op_kind == "write" {
            // `document.write` after load: append the fragment to <body>.
            if let Some(body) = find_body(doc) {
                let frag = argus_html::parse(&value);
                if let Some(fbody) = find_body(&frag) {
                    let kids: Vec<NodeId> = frag.children(fbody).collect();
                    for c in kids {
                        import_subtree(&frag, c, doc, body);
                    }
                }
            }
            continue;
        }

        let Some(node) = resolve_target(doc, &created, get("tgt")) else {
            continue;
        };
        match op_kind {
            "prop" => apply_prop(doc, node, &key, &value),
            "style" => merge_style(doc, node, &key, &value),
            "attr" => set_attribute(doc, node, &key, &value),
            "removeattr" => remove_attribute(doc, node, &key),
            "class" => apply_class_list(doc, node, &key, &value),
            "remove" => doc.detach(node),
            "append" => {
                if let Some(child) = resolve_target(doc, &created, get("child")) {
                    doc.append(node, child);
                }
            }
            "insertBefore" => {
                if let Some(child) = resolve_target(doc, &created, get("child")) {
                    match resolve_target(doc, &created, get("ref")) {
                        Some(reference) => doc.insert_before(reference, child),
                        None => doc.append(node, child),
                    }
                }
            }
            _ => {}
        }
    }
}

/// Resolve a target descriptor to a node. `id`/`sel` resolve globally, `new`
/// looks up a created element, and `scoped` finds the first descendant of a
/// (recursively resolved) parent matching a selector.
fn resolve_target(
    doc: &Document,
    created: &std::collections::HashMap<String, NodeId>,
    tgt: Option<&Json>,
) -> Option<NodeId> {
    let Some(Json::Obj(t)) = tgt else { return None };
    let f = |k: &str| t.iter().find(|(n, _)| n == k).and_then(|(_, v)| v.as_str());
    let obj = |k: &str| t.iter().find(|(n, _)| n == k).map(|(_, v)| v);
    match f("kind") {
        Some("id") => f("val").and_then(|v| find_by_id(doc, v)),
        Some("sel") => f("val").and_then(|v| find_by_selector(doc, v)),
        Some("new") => f("val").and_then(|v| created.get(v).copied()),
        Some("scoped") => {
            let parent = resolve_target(doc, created, obj("parent"))?;
            find_by_selector_within(doc, parent, f("val")?)
        }
        _ => None,
    }
}

/// The first element matching a CSS selector, in document order (`None` if the
/// selector fails to parse or nothing matches).
fn find_by_selector(doc: &Document, sel: &str) -> Option<NodeId> {
    let selectors = argus_css::selector::parse_selector_list(&argus_css::tokenizer::tokenize(sel));
    let selector = selectors.into_iter().next()?;
    fn walk(doc: &Document, n: NodeId, sel: &argus_css::Selector) -> Option<NodeId> {
        if matches!(&doc.node(n).data, NodeData::Element(_)) && argus_css::matches(doc, n, sel) {
            return Some(n);
        }
        for c in doc.children(n) {
            if let Some(found) = walk(doc, c, sel) {
                return Some(found);
            }
        }
        None
    }
    walk(doc, doc.root(), &selector)
}

/// The first descendant of `root` (excluding `root` itself) matching `sel`.
fn find_by_selector_within(doc: &Document, root: NodeId, sel: &str) -> Option<NodeId> {
    let selectors = argus_css::selector::parse_selector_list(&argus_css::tokenizer::tokenize(sel));
    let selector = selectors.into_iter().next()?;
    fn walk(doc: &Document, n: NodeId, sel: &argus_css::Selector) -> Option<NodeId> {
        if matches!(&doc.node(n).data, NodeData::Element(_)) && argus_css::matches(doc, n, sel) {
            return Some(n);
        }
        for c in doc.children(n) {
            if let Some(found) = walk(doc, c, sel) {
                return Some(found);
            }
        }
        None
    }
    doc.children(root).find_map(|c| walk(doc, c, &selector))
}

/// Interpret a JS property assignment (`el.<key> = value`).
fn apply_prop(doc: &mut Document, node: NodeId, key: &str, value: &str) {
    match key {
        "textContent" | "innerText" => set_text_content(doc, node, value),
        "innerHTML" => set_inner_html(doc, node, value),
        "className" => set_attribute(doc, node, "class", value),
        _ => set_attribute(doc, node, key, value),
    }
}

/// The first element with `id` in document order.
fn find_by_id(doc: &Document, id: &str) -> Option<NodeId> {
    fn walk(doc: &Document, n: NodeId, id: &str) -> Option<NodeId> {
        if let NodeData::Element(e) = &doc.node(n).data {
            if e.attr("id") == Some(id) {
                return Some(n);
            }
        }
        for c in doc.children(n) {
            if let Some(found) = walk(doc, c, id) {
                return Some(found);
            }
        }
        None
    }
    walk(doc, doc.root(), id)
}

/// Detach every child of `node`.
fn clear_children(doc: &mut Document, node: NodeId) {
    let kids: Vec<NodeId> = doc.children(node).collect();
    for c in kids {
        doc.detach(c);
    }
}

fn set_text_content(doc: &mut Document, node: NodeId, text: &str) {
    clear_children(doc, node);
    let t = doc.create_text(text);
    doc.append(node, t);
}

fn set_inner_html(doc: &mut Document, node: NodeId, html: &str) {
    clear_children(doc, node);
    // Parse the fragment as a document and import its <body> children.
    let frag = argus_html::parse(html);
    let Some(body) = find_body(&frag) else { return };
    let kids: Vec<NodeId> = frag.children(body).collect();
    for c in kids {
        import_subtree(&frag, c, doc, node);
    }
}

fn find_body(doc: &Document) -> Option<NodeId> {
    fn walk(doc: &Document, n: NodeId) -> Option<NodeId> {
        if let NodeData::Element(e) = &doc.node(n).data {
            if e.name.is_html("body") {
                return Some(n);
            }
        }
        for c in doc.children(n) {
            if let Some(b) = walk(doc, c) {
                return Some(b);
            }
        }
        None
    }
    walk(doc, doc.root())
}

/// Deep-copy `src_node` from `src` into `dst` as a new child of `dst_parent`.
fn import_subtree(src: &Document, src_node: NodeId, dst: &mut Document, dst_parent: NodeId) {
    let new = match &src.node(src_node).data {
        NodeData::Element(e) => dst.create_element(
            QualName::html(e.name.local.clone()),
            e.attrs
                .iter()
                .map(|a| Attribute::new(a.name.clone(), a.value.clone()))
                .collect(),
        ),
        NodeData::Text(t) => dst.create_text(t.clone()),
        NodeData::Comment(t) => dst.create_comment(t.clone()),
        _ => return,
    };
    dst.append(dst_parent, new);
    let kids: Vec<NodeId> = src.children(src_node).collect();
    for c in kids {
        import_subtree(src, c, dst, new);
    }
}

/// Apply a `classList` mutation (`add`/`remove`/`toggle`) to an element's class.
fn apply_class_list(doc: &mut Document, node: NodeId, action: &str, class: &str) {
    if class.is_empty() {
        return;
    }
    let current = doc
        .node(node)
        .as_element()
        .and_then(|e| e.attr("class"))
        .unwrap_or("")
        .to_string();
    let mut classes: Vec<&str> = current.split_whitespace().collect();
    let present = classes.contains(&class);
    let want = match action {
        "add" => true,
        "remove" => false,
        "toggle" => !present,
        _ => present,
    };
    if want && !present {
        classes.push(class);
    } else if !want && present {
        classes.retain(|c| *c != class);
    }
    set_attribute(doc, node, "class", &classes.join(" "));
}

/// Remove an attribute from an element.
fn remove_attribute(doc: &mut Document, node: NodeId, name: &str) {
    if let NodeData::Element(e) = doc.data_mut(node) {
        e.attrs.retain(|a| &*a.name != name);
    }
}

/// Set or replace an attribute on an element.
fn set_attribute(doc: &mut Document, node: NodeId, name: &str, value: &str) {
    if let NodeData::Element(e) = doc.data_mut(node) {
        if let Some(a) = e.attrs.iter_mut().find(|a| &*a.name == name) {
            a.value = value.to_string();
        } else {
            e.attrs.push(Attribute::new(name, value));
        }
    }
}

/// Merge `prop: value` into the element's inline `style` attribute, converting a
/// camelCase JS property (e.g. `backgroundColor`) to its CSS form.
fn merge_style(doc: &mut Document, node: NodeId, prop: &str, value: &str) {
    let css_name = camel_to_kebab(prop);
    let existing = doc
        .node(node)
        .as_element()
        .and_then(|e| e.attr("style"))
        .unwrap_or("")
        .to_string();
    let mut decls: Vec<(String, String)> = Vec::new();
    for decl in existing.split(';') {
        if let Some((k, v)) = decl.split_once(':') {
            let k = k.trim();
            if !k.is_empty() && k != css_name {
                decls.push((k.to_string(), v.trim().to_string()));
            }
        }
    }
    decls.push((css_name, value.trim().to_string()));
    let merged = decls
        .iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect::<Vec<_>>()
        .join("; ");
    set_attribute(doc, node, "style", &merged);
}

fn camel_to_kebab(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for ch in s.chars() {
        if ch.is_ascii_uppercase() {
            out.push('-');
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

// ---- Minimal JSON --------------------------------------------------------

/// A parsed JSON value (only what the ops payload needs). `Bool`/`Num` are parsed
/// for completeness but the ops payload only reads strings.
#[allow(dead_code)]
enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// Escape a string as a JSON string literal (with surrounding quotes).
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn parse_json(s: &str) -> Option<Json> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let v = parse_value(&chars, &mut i)?;
    Some(v)
}

fn skip_ws(c: &[char], i: &mut usize) {
    while *i < c.len() && c[*i].is_whitespace() {
        *i += 1;
    }
}

fn parse_value(c: &[char], i: &mut usize) -> Option<Json> {
    skip_ws(c, i);
    match c.get(*i)? {
        '"' => parse_string(c, i).map(Json::Str),
        '[' => parse_array(c, i),
        '{' => parse_object(c, i),
        't' => parse_literal(c, i, "true", Json::Bool(true)),
        'f' => parse_literal(c, i, "false", Json::Bool(false)),
        'n' => parse_literal(c, i, "null", Json::Null),
        _ => parse_number(c, i),
    }
}

fn parse_literal(c: &[char], i: &mut usize, word: &str, val: Json) -> Option<Json> {
    for ch in word.chars() {
        if c.get(*i) != Some(&ch) {
            return None;
        }
        *i += 1;
    }
    Some(val)
}

fn parse_string(c: &[char], i: &mut usize) -> Option<String> {
    if c.get(*i) != Some(&'"') {
        return None;
    }
    *i += 1;
    let mut out = String::new();
    while let Some(&ch) = c.get(*i) {
        *i += 1;
        match ch {
            '"' => return Some(out),
            '\\' => {
                let esc = *c.get(*i)?;
                *i += 1;
                match esc {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'u' => {
                        let mut code = 0u32;
                        for _ in 0..4 {
                            code = code * 16 + c.get(*i)?.to_digit(16)?;
                            *i += 1;
                        }
                        out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                    }
                    _ => return None,
                }
            }
            _ => out.push(ch),
        }
    }
    None
}

fn parse_array(c: &[char], i: &mut usize) -> Option<Json> {
    *i += 1; // '['
    let mut out = Vec::new();
    skip_ws(c, i);
    if c.get(*i) == Some(&']') {
        *i += 1;
        return Some(Json::Arr(out));
    }
    loop {
        out.push(parse_value(c, i)?);
        skip_ws(c, i);
        match c.get(*i) {
            Some(',') => {
                *i += 1;
            }
            Some(']') => {
                *i += 1;
                return Some(Json::Arr(out));
            }
            _ => return None,
        }
    }
}

fn parse_object(c: &[char], i: &mut usize) -> Option<Json> {
    *i += 1; // '{'
    let mut out = Vec::new();
    skip_ws(c, i);
    if c.get(*i) == Some(&'}') {
        *i += 1;
        return Some(Json::Obj(out));
    }
    loop {
        skip_ws(c, i);
        let key = parse_string(c, i)?;
        skip_ws(c, i);
        if c.get(*i) != Some(&':') {
            return None;
        }
        *i += 1;
        let val = parse_value(c, i)?;
        out.push((key, val));
        skip_ws(c, i);
        match c.get(*i) {
            Some(',') => {
                *i += 1;
            }
            Some('}') => {
                *i += 1;
                return Some(Json::Obj(out));
            }
            _ => return None,
        }
    }
}

fn parse_number(c: &[char], i: &mut usize) -> Option<Json> {
    let start = *i;
    while let Some(&ch) = c.get(*i) {
        if ch.is_ascii_digit() || matches!(ch, '-' | '+' | '.' | 'e' | 'E') {
            *i += 1;
        } else {
            break;
        }
    }
    let s: String = c[start..*i].iter().collect();
    s.parse().ok().map(Json::Num)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(doc: &Document, id: &str) -> String {
        let node = find_by_id(doc, id).expect("element");
        text_content(doc, node)
    }
    fn attr_of(doc: &Document, id: &str, name: &str) -> Option<String> {
        find_by_id(doc, id).and_then(|n| {
            doc.node(n)
                .as_element()
                .and_then(|e| e.attr(name))
                .map(String::from)
        })
    }

    #[test]
    fn set_text_content_via_get_element_by_id() {
        let mut doc = argus_html::parse(
            "<div id=out>old</div>\
             <script>document.getElementById('out').textContent = 'new text';</script>",
        );
        apply_scripts(&mut doc);
        assert_eq!(text_of(&doc, "out"), "new text");
    }

    #[test]
    fn read_modify_write_uses_seeded_value() {
        // The seeded textContent lets a script read the old value and append.
        let mut doc = argus_html::parse(
            "<span id=c>5</span>\
             <script>var e=document.getElementById('c'); e.textContent = e.textContent + '0';</script>",
        );
        apply_scripts(&mut doc);
        assert_eq!(text_of(&doc, "c"), "50");
    }

    #[test]
    fn style_and_attribute_and_class_mutations() {
        let mut doc = argus_html::parse(
            "<div id=b>x</div>\
             <script>\
               var e=document.getElementById('b');\
               e.style.color = 'red';\
               e.style.backgroundColor = 'blue';\
               e.className = 'active';\
               e.setAttribute('data-k', 'v');\
             </script>",
        );
        apply_scripts(&mut doc);
        let style = attr_of(&doc, "b", "style").unwrap_or_default();
        assert!(style.contains("color: red"), "style: {style}");
        assert!(style.contains("background-color: blue"), "style: {style}");
        assert_eq!(attr_of(&doc, "b", "class").as_deref(), Some("active"));
        assert_eq!(attr_of(&doc, "b", "data-k").as_deref(), Some("v"));
    }

    #[test]
    fn inner_html_replaces_children() {
        let mut doc = argus_html::parse(
            "<div id=h>old</div>\
             <script>document.getElementById('h').innerHTML = '<b>bold</b> and <i>italic</i>';</script>",
        );
        apply_scripts(&mut doc);
        let node = find_by_id(&doc, "h").unwrap();
        // The text content is now the fragment's text; a <b> child exists.
        assert_eq!(text_of(&doc, "h"), "bold and italic");
        let has_b = doc
            .children(node)
            .any(|c| matches!(&doc.node(c).data, NodeData::Element(e) if e.name.is_html("b")));
        assert!(has_b, "innerHTML should create a <b> element");
    }

    #[test]
    fn query_selector_by_class_and_tag() {
        let mut doc = argus_html::parse(
            "<p class=\"intro\" id=\"p1\">a</p><p id=\"p2\">b</p>\
             <script>\
               document.querySelector('.intro').textContent = 'X';\
               document.querySelector('p:nth-child(2)').textContent = 'Y';\
             </script>",
        );
        apply_scripts(&mut doc);
        assert_eq!(text_of(&doc, "p1"), "X");
        // The 2nd <p> in document order is p2.
        assert_eq!(text_of(&doc, "p2"), "Y");
    }

    #[test]
    fn create_element_and_append_child() {
        let mut doc = argus_html::parse(
            "<div id=\"root\"></div>\
             <script>\
               var p = document.createElement('p');\
               p.textContent = 'created node';\
               p.className = 'made';\
               document.getElementById('root').appendChild(p);\
             </script>",
        );
        apply_scripts(&mut doc);
        let root = find_by_id(&doc, "root").unwrap();
        let child = doc.children(root).next().expect("appended child");
        let e = doc.node(child).as_element().expect("element");
        assert!(e.name.is_html("p"));
        assert_eq!(e.attr("class"), Some("made"));
        assert_eq!(text_content(&doc, child), "created node");
    }

    #[test]
    fn class_list_add_remove_toggle() {
        let mut doc = argus_html::parse(
            "<div id=\"d\" class=\"a b\">x</div>\
             <script>\
               var e = document.getElementById('d');\
               e.classList.add('c');\
               e.classList.remove('a');\
               e.classList.toggle('b');\
               e.classList.toggle('z');\
             </script>",
        );
        apply_scripts(&mut doc);
        let class = attr_of(&doc, "d", "class").unwrap_or_default();
        let set: std::collections::BTreeSet<&str> = class.split_whitespace().collect();
        // start a,b → +c, -a, toggle b (off), toggle z (on) → {c, z}
        assert_eq!(
            set,
            ["c", "z"]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
            "class was {class:?}"
        );
    }

    #[test]
    fn scoped_element_query_selector() {
        // el.querySelector resolves within the element's own subtree.
        let mut doc = argus_html::parse(
            "<div id=\"a\"><span class=\"x\" id=\"ax\">1</span></div>\
             <div id=\"b\"><span class=\"x\" id=\"bx\">2</span></div>\
             <script>\
               document.getElementById('b').querySelector('.x').textContent = 'hit';\
             </script>",
        );
        apply_scripts(&mut doc);
        // Only the .x inside #b is changed; the one in #a is untouched.
        assert_eq!(text_of(&doc, "bx"), "hit");
        assert_eq!(text_of(&doc, "ax"), "1");
    }

    #[test]
    fn insert_before_orders_children() {
        let mut doc = argus_html::parse(
            "<ul id=\"l\"><li id=\"ref\">existing</li></ul>\
             <script>\
               var li = document.createElement('li');\
               li.textContent = 'inserted';\
               li.setAttribute('id', 'ins');\
               document.getElementById('l').insertBefore(li, document.getElementById('ref'));\
             </script>",
        );
        apply_scripts(&mut doc);
        let list = find_by_id(&doc, "l").unwrap();
        let order: Vec<String> = doc
            .children(list)
            .filter_map(|c| {
                doc.node(c)
                    .as_element()
                    .and_then(|e| e.attr("id"))
                    .map(String::from)
            })
            .collect();
        assert_eq!(
            order,
            vec!["ins", "ref"],
            "inserted node should precede ref"
        );
    }

    #[test]
    fn document_write_appends_to_body() {
        let mut doc = argus_html::parse(
            "<p id=\"first\">a</p>\
             <script>document.write('<p id=\"w\">written</p>');</script>",
        );
        apply_scripts(&mut doc);
        // The written element exists with its text.
        assert_eq!(text_of(&doc, "w"), "written");
    }

    #[test]
    fn apply_ops_survives_arbitrary_json() {
        // The ops payload comes from the JS VM; the JSON parser + op replay must
        // never panic on malformed or hostile input.
        let mut seed = 0x2545F4914F6CDD1Du64;
        let mut byte = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed & 0xff) as u8
        };
        const BIAS: &[u8] = b"[]{}\",:opprstyleattrclassappendcreatetgtkindidselval0\\u\n ";
        for _ in 0..3000 {
            let len = (byte() as usize) * 2;
            let s: String = (0..len)
                .map(|_| {
                    if byte() < 170 {
                        BIAS[byte() as usize % BIAS.len()] as char
                    } else {
                        byte() as char
                    }
                })
                .collect();
            let mut doc = argus_html::parse("<div id=d><span id=s>x</span></div>");
            let mut storage = std::collections::HashMap::new();
            apply_ops(&mut doc, &s, &mut storage); // must not panic
            let _ = doc.serialize();
        }
    }

    #[test]
    fn event_replay_accumulates_state() {
        // A click counter: JS variable state must accumulate across replayed
        // clicks (showing N after N clicks, not 1) — the core of the model.
        let html = "<button id=\"btn\">+</button><span id=\"count\">0</span>\
             <script>\
               var c = 0;\
               document.getElementById('btn').addEventListener('click', function(e){\
                 c = c + 1;\
                 document.getElementById('count').textContent = '' + c;\
               });\
             </script>";
        let click = Interaction {
            kind: "id".into(),
            val: "btn".into(),
            event: "click".into(),
        };
        // Replay three clicks.
        let mut doc = argus_html::parse(html);
        apply_scripts_with_events(&mut doc, &[click.clone(), click.clone(), click.clone()]);
        assert_eq!(text_of(&doc, "count"), "3");

        // Zero clicks → handler never fires; the count stays its initial value.
        let mut doc0 = argus_html::parse(html);
        apply_scripts_with_events(&mut doc0, &[]);
        assert_eq!(text_of(&doc0, "count"), "0");
    }

    #[test]
    fn event_replay_dom_readback_via_overlay() {
        // DOM-backed state: the handler reads textContent and increments it; the
        // overlay must make each replayed click see the previous click's write.
        let html = "<span id=\"n\">10</span>\
             <script>\
               document.getElementById('n').onclick = function(){\
                 var e = document.getElementById('n');\
                 e.textContent = '' + (parseInt(e.textContent) + 1);\
               };\
             </script>";
        let click = Interaction {
            kind: "id".into(),
            val: "n".into(),
            event: "click".into(),
        };
        let mut doc = argus_html::parse(html);
        apply_scripts_with_events(&mut doc, &[click.clone(), click.clone()]);
        assert_eq!(text_of(&doc, "n"), "12");
    }

    #[test]
    fn local_storage_set_then_read_within_session() {
        // localStorage is consistent within the run (a click reads what init wrote).
        let html = "<div id=\"out\"></div>\
             <script>\
               localStorage.setItem('greeting', 'hi');\
               document.getElementById('out').onclick = function(){\
                 document.getElementById('out').textContent = localStorage.getItem('greeting');\
               };\
             </script>";
        let click = Interaction {
            kind: "id".into(),
            val: "out".into(),
            event: "click".into(),
        };
        let mut doc = argus_html::parse(html);
        apply_scripts_with_events(&mut doc, &[click]);
        assert_eq!(text_of(&doc, "out"), "hi");
    }

    #[test]
    fn local_storage_persists_across_navigations() {
        // A shared storage map (as the content process keeps) carries localStorage
        // writes from one page load to the next.
        let mut storage = std::collections::HashMap::new();

        // Page 1 writes a value.
        let mut page1 = argus_html::parse(
            "<div id=\"a\"></div><script>localStorage.setItem('user', 'mark');</script>",
        );
        apply_scripts_session(&mut page1, &[], &mut storage);
        assert_eq!(storage.get("user").map(String::as_str), Some("mark"));

        // Page 2 (a fresh document) reads it back through the persisted map.
        let mut page2 = argus_html::parse(
            "<div id=\"b\"></div>\
             <script>document.getElementById('b').textContent = \
               localStorage.getItem('user') || 'none';</script>",
        );
        apply_scripts_session(&mut page2, &[], &mut storage);
        assert_eq!(text_of(&page2, "b"), "mark");
    }

    #[test]
    fn set_timeout_runs_synchronously_in_delay_order() {
        // Timers drain after the script, earliest delay first. Here the 10ms timer
        // appends "B" before the 50ms timer appends "C"; "A" is set inline first.
        let mut doc = argus_html::parse(
            "<div id=\"out\"></div>\
             <script>\
               var e = document.getElementById('out');\
               e.textContent = 'A';\
               setTimeout(function(){ e.textContent = e.textContent + 'C'; }, 50);\
               setTimeout(function(){ e.textContent = e.textContent + 'B'; }, 10);\
             </script>",
        );
        apply_scripts(&mut doc);
        assert_eq!(text_of(&doc, "out"), "ABC");
    }

    #[test]
    fn async_promise_mutations_are_reconciled() {
        // A DOM write inside a Promise.then callback must land: the engine's event
        // loop drains the microtask before the followup reads __argus_ops.
        let mut doc = argus_html::parse(
            "<div id=\"out\">start</div>\
             <script>\
               Promise.resolve('done').then(function(v){\
                 document.getElementById('out').textContent = v;\
               });\
             </script>",
        );
        apply_scripts(&mut doc);
        assert_eq!(text_of(&doc, "out"), "done");
    }

    #[test]
    fn async_await_mutations_are_reconciled() {
        // async/await: the write happens after an awaited promise resolves, in a
        // continuation the native event loop runs.
        let mut doc = argus_html::parse(
            "<div id=\"out\">x</div>\
             <script>\
               (async function(){\
                 var v = await Promise.resolve('async-ok');\
                 document.getElementById('out').textContent = v;\
               })();\
             </script>",
        );
        apply_scripts(&mut doc);
        assert_eq!(text_of(&doc, "out"), "async-ok");
    }

    #[test]
    fn request_animation_frame_runs_deferred_dom_init() {
        // rAF callbacks are drained like timers; a write inside one must land, and
        // the callback receives a (synthetic) numeric timestamp.
        let mut doc = argus_html::parse(
            "<div id=\"out\">pending</div>\
             <script>\
               requestAnimationFrame(function(ts){\
                 document.getElementById('out').textContent = (typeof ts === 'number') ? 'frame' : 'no-ts';\
               });\
             </script>",
        );
        apply_scripts(&mut doc);
        assert_eq!(text_of(&doc, "out"), "frame");
    }

    #[test]
    fn setting_input_value_updates_the_value_attribute() {
        // `input.value = ...` sets the `value` attribute, which the layout renders.
        let mut doc = argus_html::parse(
            "<input id=\"f\" value=\"old\">\
             <script>document.getElementById('f').value = 'typed text';</script>",
        );
        apply_scripts(&mut doc);
        assert_eq!(attr_of(&doc, "f", "value").as_deref(), Some("typed text"));
    }

    #[test]
    fn remove_attribute_works() {
        // A script can toggle a boolean attribute (e.g. open a <details>).
        let mut doc = argus_html::parse(
            "<details id=\"d\"><summary>S</summary>body</details>\
             <button id=\"b\" disabled>x</button>\
             <script>\
               document.getElementById('d').setAttribute('open', '');\
               document.getElementById('b').removeAttribute('disabled');\
             </script>",
        );
        apply_scripts(&mut doc);
        assert!(attr_of(&doc, "d", "open").is_some(), "details opened");
        assert!(attr_of(&doc, "b", "disabled").is_none(), "button enabled");
    }

    #[test]
    fn csp_meta_blocks_inline_scripts() {
        // A restrictive CSP stops the inline script from mutating the DOM.
        let blocked = "<meta http-equiv=\"Content-Security-Policy\" content=\"script-src 'self'\">\
             <div id=\"o\">before</div>\
             <script>document.getElementById('o').textContent = 'after';</script>";
        let mut doc = argus_html::parse(blocked);
        apply_scripts(&mut doc);
        assert_eq!(text_of(&doc, "o"), "before", "CSP should block the script");

        // 'unsafe-inline' permits it; no CSP permits it.
        for policy in [
            "<meta http-equiv=\"Content-Security-Policy\" content=\"script-src 'unsafe-inline'\">",
            "",
        ] {
            let html = format!(
                "{policy}<div id=\"o\">before</div>\
                 <script>document.getElementById('o').textContent = 'after';</script>"
            );
            let mut doc = argus_html::parse(&html);
            apply_scripts(&mut doc);
            assert_eq!(
                text_of(&doc, "o"),
                "after",
                "script should run for: {policy:?}"
            );
        }
    }

    #[test]
    fn no_scripts_is_noop() {
        let mut doc = argus_html::parse("<p id=p>hi</p>");
        assert!(apply_scripts(&mut doc).is_none());
        assert_eq!(text_of(&doc, "p"), "hi");
    }
}

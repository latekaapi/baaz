//! The tools of docs/27 §6.1, one `async fn` each. The service chooses the tab,
//! checks URLs and owns tab lifecycle; everything here talks to one page.

use std::time::Duration;

use serde_json::{json, Value};

use super::policy::{self, Reason, Target};
use super::snapshot::{self, FrameTree, SnapshotOptions};
use super::{Ctx, PersonAsk};

/// How long `browser_navigate` waits for the load event.
const LOAD_TIMEOUT: Duration = Duration::from_secs(20);
/// How long an action waits for the page to go quiet before reporting.
const SETTLE_MAX: Duration = Duration::from_secs(3);
/// No request in flight for this long counts as settled.
const QUIET: Duration = Duration::from_millis(400);
const POLL: Duration = Duration::from_millis(50);

/// Turns on what the log reads, once per tab.
pub async fn ensure_domains(ctx: &Ctx) -> Result<(), String> {
    if ctx.tab.borrow().domains_on {
        return Ok(());
    }
    for domain in ["Page", "Runtime", "Network", "Log", "DOM", "Accessibility"] {
        ctx.call(&format!("{domain}.enable"), json!({})).await?;
    }
    // A session's tab usually isn't on screen; the page should behave as if it were.
    let _ = ctx.call("Emulation.setFocusEmulationEnabled", json!({"enabled": true})).await;
    ctx.tab.borrow_mut().domains_on = true;
    Ok(())
}

async fn evaluate(ctx: &Ctx, body: &str) -> Result<Value, String> {
    let expression = format!("(async () => {{\n{body}\n}})()");
    let answer = ctx
        .call(
            "Runtime.evaluate",
            json!({"expression": expression, "awaitPromise": true, "returnByValue": true, "userGesture": true}),
        )
        .await?;
    if let Some(details) = answer.get("exceptionDetails") {
        let message = details
            .pointer("/exception/description")
            .or_else(|| details.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("the script failed");
        return Err(message.lines().next().unwrap_or(message).to_owned());
    }
    Ok(answer.pointer("/result/value").cloned().unwrap_or(Value::Null))
}

async fn page_url(ctx: &Ctx) -> Result<String, String> {
    Ok(evaluate(ctx, "return location.href").await?.as_str().unwrap_or("").to_owned())
}

/// Reads every frame's tree and renders it.
async fn read_tree(ctx: &Ctx, options: &SnapshotOptions) -> Result<snapshot::Rendered, String> {
    let tree = ctx.call("Page.getFrameTree", json!({})).await?;
    let mut frames: Vec<(Option<String>, Option<i64>)> = Vec::new();
    collect_frames(tree.get("frameTree").unwrap_or(&Value::Null), true, &mut frames);
    let mut owned = Vec::with_capacity(frames.len());
    for (frame_id, _) in &frames {
        let params = match frame_id {
            Some(id) => json!({"frameId": id}),
            None => json!({}),
        };
        // A frame that went away (or lives in another process) is left out, never fatal.
        let nodes = ctx.call("Accessibility.getFullAXTree", params).await;
        let owner = match (&frame_id, nodes.is_ok(), frames.first().map(|first| &first.0 == frame_id)) {
            (Some(id), true, Some(false)) => ctx
                .call("DOM.getFrameOwner", json!({"frameId": id}))
                .await
                .ok()
                .and_then(|answer| answer.get("backendNodeId").and_then(Value::as_i64)),
            _ => None,
        };
        let is_main = frames.first().map(|first| &first.0 == frame_id).unwrap_or(true);
        if let Ok(answer) = nodes {
            if is_main || owner.is_some() {
                let list = answer.get("nodes").and_then(Value::as_array).cloned().unwrap_or_default();
                owned.push((list, owner));
            }
        } else if is_main {
            return Err(nodes.err().unwrap_or_default());
        }
    }
    let views: Vec<FrameTree<'_>> = owned.iter().map(|(nodes, owner)| FrameTree { nodes, owner: *owner }).collect();
    let mut agent = ctx.tab.borrow_mut();
    Ok(snapshot::render(&views, &mut agent.refs, options))
}

fn collect_frames(tree: &Value, main: bool, out: &mut Vec<(Option<String>, Option<i64>)>) {
    let id = tree.pointer("/frame/id").and_then(Value::as_str).map(str::to_owned);
    if main || id.is_some() {
        out.push((id, None));
    }
    if let Some(children) = tree.get("childFrames").and_then(Value::as_array) {
        for child in children {
            collect_frames(child, false, out);
        }
    }
}

/// `browser_snapshot`.
pub async fn snapshot(ctx: Ctx, params: Value) -> Result<Value, String> {
    ensure_domains(&ctx).await?;
    let options = SnapshotOptions {
        interactive: params.get("interactive").and_then(Value::as_bool).unwrap_or(false),
        scope: params.get("scope").and_then(Value::as_str).map(str::to_owned),
        max_chars: params.get("max_chars").and_then(Value::as_u64).unwrap_or(0) as usize,
        cursor: params.get("cursor").and_then(Value::as_u64).unwrap_or(0) as usize,
    };
    let delta = params.get("delta").and_then(Value::as_bool).unwrap_or(false);
    let shot = read_tree(&ctx, &options).await?;
    let previous = std::mem::replace(&mut ctx.tab.borrow_mut().last_snapshot, shot.lines.clone());
    let body = if delta { snapshot::diff(&previous, &shot.lines).join("\n") } else { shot.text };
    let mut out = json!({"snapshot": snapshot::fence(&body)});
    if let Some(cursor) = shot.next_cursor.filter(|_| !delta) {
        out["next_cursor"] = json!(cursor);
    }
    Ok(out)
}

/// `browser_navigate`, after the service has checked the URL.
pub async fn navigate(ctx: Ctx, url: String) -> Result<Value, String> {
    ensure_domains(&ctx).await?;
    let before = ctx.tab.borrow().log.loads();
    let answer = ctx.call("Page.navigate", json!({"url": url})).await?;
    if let Some(error) = answer.get("errorText").and_then(Value::as_str) {
        return Err(format!("could not open {url}: {error}"));
    }
    // A same-document navigation fires no load event: there is no loader to wait for.
    let same_document = answer.get("loaderId").is_none();
    let started = ctx.now();
    while !same_document && ctx.tab.borrow().log.loads() == before {
        if ctx.now().duration_since(started) > LOAD_TIMEOUT {
            return Err(format!("{url} did not finish loading in {} s", LOAD_TIMEOUT.as_secs()));
        }
        ctx.sleep(POLL).await;
    }
    let info = evaluate(&ctx, "return {url: location.href, title: document.title}").await?;
    Ok(info)
}

/// Waits until no request has been in flight for [`QUIET`], at most [`SETTLE_MAX`].
async fn settle(ctx: &Ctx) {
    let started = ctx.now();
    let mut quiet_since = None;
    loop {
        ctx.sleep(POLL).await;
        let now = ctx.now();
        if ctx.tab.borrow().log.in_flight() > 0 {
            quiet_since = None;
        } else if now.duration_since(*quiet_since.get_or_insert(now)) >= QUIET {
            return;
        }
        if now.duration_since(started) >= SETTLE_MAX {
            return;
        }
    }
}

/// Where an action lands: a ref's centre, or the caller's `x`,`y`.
async fn point_of(ctx: &Ctx, node: Option<i64>, params: &Value) -> Result<(f64, f64), String> {
    if let Some(node) = node {
        // Best effort: a node that cannot scroll may still have a box.
        let _ = ctx.call("DOM.scrollIntoViewIfNeeded", json!({"backendNodeId": node})).await;
        let answer = ctx.call("DOM.getBoxModel", json!({"backendNodeId": node})).await?;
        let quad: Vec<f64> = answer
            .pointer("/model/content")
            .and_then(Value::as_array)
            .map(|quad| quad.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default();
        if quad.len() != 8 {
            return Err(String::from("that element has no box on the page"));
        }
        let x = (quad[0] + quad[2] + quad[4] + quad[6]) / 4.0;
        let y = (quad[1] + quad[3] + quad[5] + quad[7]) / 4.0;
        return Ok((x, y));
    }
    match (params.get("x").and_then(Value::as_f64), params.get("y").and_then(Value::as_f64)) {
        (Some(x), Some(y)) => Ok((x, y)),
        _ => Err(String::from("this action needs a ref, or x and y")),
    }
}

async fn mouse(ctx: &Ctx, kind: &str, x: f64, y: f64, button: &str, clicks: i64) -> Result<(), String> {
    let buttons = i64::from(kind == "mousePressed" && button != "none");
    ctx.call(
        "Input.dispatchMouseEvent",
        json!({"type": kind, "x": x, "y": y, "button": button, "buttons": buttons, "clickCount": clicks}),
    )
    .await?;
    Ok(())
}

async fn click_at(ctx: &Ctx, x: f64, y: f64) -> Result<(), String> {
    mouse(ctx, "mouseMoved", x, y, "none", 0).await?;
    mouse(ctx, "mousePressed", x, y, "left", 1).await?;
    mouse(ctx, "mouseReleased", x, y, "left", 1).await
}

/// One key: `Enter`, `Tab`, `Escape`, an arrow or a printable character.
fn key_event(key: &str) -> Value {
    let (code, vk, text) = match key {
        "Enter" => ("Enter", 13, "\r"),
        "Tab" => ("Tab", 9, ""),
        "Escape" => ("Escape", 27, ""),
        "Backspace" => ("Backspace", 8, ""),
        "Delete" => ("Delete", 46, ""),
        "ArrowUp" => ("ArrowUp", 38, ""),
        "ArrowDown" => ("ArrowDown", 40, ""),
        "ArrowLeft" => ("ArrowLeft", 37, ""),
        "ArrowRight" => ("ArrowRight", 39, ""),
        other => return json!({"key": other, "text": other}),
    };
    json!({"key": code, "code": code, "windowsVirtualKeyCode": vk, "text": text})
}

async fn press(ctx: &Ctx, key: &str) -> Result<(), String> {
    let mut event = key_event(key);
    event["type"] = json!("keyDown");
    ctx.call("Input.dispatchKeyEvent", event.clone()).await?;
    event["type"] = json!("keyUp");
    event.as_object_mut().map(|fields| fields.remove("text"));
    ctx.call("Input.dispatchKeyEvent", event).await?;
    Ok(())
}

/// What the policy needs about `node` (or the focused element, for a key press).
async fn inspect(ctx: &Ctx, node: Option<i64>, snapshot_name: &str) -> Target {
    let object = match node {
        Some(node) => ctx
            .call("DOM.resolveNode", json!({"backendNodeId": node}))
            .await
            .ok()
            .and_then(|answer| answer.pointer("/object/objectId").and_then(Value::as_str).map(str::to_owned)),
        None => ctx
            .call("Runtime.evaluate", json!({"expression": "document.activeElement"}))
            .await
            .ok()
            .and_then(|answer| answer.pointer("/result/objectId").and_then(Value::as_str).map(str::to_owned)),
    };
    let Some(object) = object else {
        return Target { name: snapshot_name.to_owned(), ..Target::default() };
    };
    let answer = ctx
        .call(
            "Runtime.callFunctionOn",
            json!({"functionDeclaration": policy::INSPECT_FUNCTION, "objectId": object, "returnByValue": true}),
        )
        .await;
    match answer.ok().as_ref().and_then(|answer| answer.pointer("/result/value")) {
        Some(value) => policy::target_from(value, snapshot_name),
        None => Target { name: snapshot_name.to_owned(), ..Target::default() },
    }
}

/// Stops for the person when `reason` applies. `Ok(None)` means go on.
async fn gate(ctx: &Ctx, target: &Target) -> Option<Value> {
    let reason = policy::classify(target)?;
    let allowed = ask(ctx, reason, &target.name).await;
    (!allowed).then(|| policy::stopped_answer(reason, target))
}

async fn ask(ctx: &Ctx, reason: Reason, detail: &str) -> bool {
    ctx.ask_person(PersonAsk { kind: reason.word().to_owned(), detail: detail.to_owned(), approval: true }).await
}

/// `browser_act`: one trusted input by ref or x,y, then the settled diff.
pub async fn act(ctx: Ctx, params: Value) -> Result<Value, String> {
    ensure_domains(&ctx).await?;
    let action = params.get("action").and_then(Value::as_str).unwrap_or("click").to_owned();
    let reference = params.get("ref").and_then(Value::as_str);
    let node = match reference {
        Some(reference) => Some(
            ctx.tab
                .borrow()
                .refs
                .node_for(reference)
                .ok_or_else(|| format!("unknown ref {reference}: take a new snapshot (refs end when the page navigates)"))?,
        ),
        None => None,
    };
    let before = ctx.tab.borrow().last_snapshot.clone();
    // What asks is what would commit: a click, a value going into a sensitive field,
    // a key press that could submit, or a file input.
    let check = matches!(action.as_str(), "click" | "press" | "type" | "fill" | "select");
    if check {
        let mut target = inspect(&ctx, node, "").await;
        // Enter on a field submits its form, whatever the field is called.
        if action == "press" && params.get("key").and_then(Value::as_str) != Some("Enter") {
            target = Target::default();
        }
        // Typing into an ordinary field commits nothing; only the field's own kind can.
        if matches!(action.as_str(), "type" | "fill" | "select")
            && !matches!(target.input_type.as_str(), "password" | "file")
        {
            target = Target::default();
        }
        if let Some(stopped) = gate(&ctx, &target).await {
            return Ok(stopped);
        }
    }
    {
        // What the action did is judged from here: an earlier navigation is not its own.
        let mut agent = ctx.tab.borrow_mut();
        agent.log.take_navigated();
        agent.log.mark_action();
    }
    match action.as_str() {
        "click" => {
            let (x, y) = point_of(&ctx, node, &params).await?;
            click_at(&ctx, x, y).await?;
        }
        "hover" => {
            let (x, y) = point_of(&ctx, node, &params).await?;
            mouse(&ctx, "mouseMoved", x, y, "none", 0).await?;
        }
        "type" | "fill" => {
            let text = params.get("text").and_then(Value::as_str).ok_or("this action needs text")?.to_owned();
            if let Some(node) = node {
                ctx.call("DOM.focus", json!({"backendNodeId": node})).await?;
            }
            if action == "fill" {
                let select_all = json!({"type": "keyDown", "key": "a", "code": "KeyA", "windowsVirtualKeyCode": 65,
                    "modifiers": 4, "commands": ["selectAll"]});
                ctx.call("Input.dispatchKeyEvent", select_all).await?;
                ctx.call("Input.dispatchKeyEvent", json!({"type": "keyUp", "key": "a", "code": "KeyA", "modifiers": 4})).await?;
            }
            ctx.call("Input.insertText", json!({"text": text})).await?;
        }
        "press" => {
            let key = params.get("key").and_then(Value::as_str).ok_or("press needs key")?.to_owned();
            if let Some(node) = node {
                ctx.call("DOM.focus", json!({"backendNodeId": node})).await?;
            }
            press(&ctx, &key).await?;
        }
        "select" => {
            let value = params.get("value").and_then(Value::as_str).ok_or("select needs value")?.to_owned();
            let node = node.ok_or("select needs a ref")?;
            let object = ctx.call("DOM.resolveNode", json!({"backendNodeId": node})).await?;
            let object = object.pointer("/object/objectId").and_then(Value::as_str).ok_or("that element is gone")?.to_owned();
            ctx.call(
                "Runtime.callFunctionOn",
                json!({"objectId": object, "arguments": [{"value": value}], "functionDeclaration":
                    "function (v) { this.value = v; this.dispatchEvent(new Event('input', {bubbles:true})); this.dispatchEvent(new Event('change', {bubbles:true})); }"}),
            )
            .await?;
        }
        "scroll" => {
            let (x, y) = point_of(&ctx, node, &params).await.unwrap_or((200.0, 200.0));
            let delta_x = params.get("delta_x").and_then(Value::as_f64).unwrap_or(0.0);
            let delta_y = params.get("delta_y").and_then(Value::as_f64).unwrap_or(400.0);
            ctx.call(
                "Input.dispatchMouseEvent",
                json!({"type": "mouseWheel", "x": x, "y": y, "deltaX": delta_x, "deltaY": delta_y}),
            )
            .await?;
        }
        "drag" => {
            let (x, y) = point_of(&ctx, node, &params).await?;
            let to = params.get("to").cloned().unwrap_or(Value::Null);
            let to_node = to.get("ref").and_then(Value::as_str).and_then(|r| ctx.tab.borrow().refs.node_for(r));
            let (to_x, to_y) = point_of(&ctx, to_node, &to).await?;
            mouse(&ctx, "mouseMoved", x, y, "none", 0).await?;
            mouse(&ctx, "mousePressed", x, y, "left", 1).await?;
            mouse(&ctx, "mouseMoved", to_x, to_y, "left", 0).await?;
            mouse(&ctx, "mouseReleased", to_x, to_y, "left", 1).await?;
        }
        other => return Err(format!("unknown action {other}")),
    }
    settle(&ctx).await;
    let shot = read_tree(&ctx, &SnapshotOptions::default()).await?;
    let changes = snapshot::diff(&before, &shot.lines);
    let (navigated, wrote, requests) = {
        let mut agent = ctx.tab.borrow_mut();
        agent.last_snapshot = shot.lines;
        (agent.log.take_navigated(), agent.log.wrote_since_action(), agent.log.requests_since_action())
    };
    let effect = if navigated || wrote {
        "committed"
    } else if changes.is_empty() && requests == 0 {
        "none"
    } else {
        "unknown"
    };
    Ok(json!({"effect": effect, "navigated": navigated, "changes": snapshot::fence(&changes.join("\n"))}))
}

/// `browser_read`.
pub async fn read(ctx: Ctx, params: Value) -> Result<Value, String> {
    let format = params.get("format").and_then(Value::as_str).unwrap_or("text");
    let max = params.get("max_chars").and_then(Value::as_u64).unwrap_or(8000);
    let body = match format {
        "links" => "return Array.from(document.links).slice(0, 500).map((a) => ({href: a.href, text: (a.innerText || '').trim().slice(0, 200)}))".to_owned(),
        "markdown" => format!("return {READ_MARKDOWN}"),
        _ => "return (document.body && document.body.innerText) || ''".to_owned(),
    };
    let answer = evaluate(&ctx, &body).await?;
    let info = evaluate(&ctx, "return {url: location.href, title: document.title}").await?;
    if format == "links" {
        return Ok(json!({"url": info["url"], "links": answer}));
    }
    let text: String = answer.as_str().unwrap_or("").chars().take(max as usize).collect();
    Ok(json!({"url": info["url"], "title": info["title"], "text": snapshot::fence(&text)}))
}

const READ_MARKDOWN: &str = r#"(function walk(n) {
  if (n.nodeType === 3) return n.textContent.replace(/\s+/g, ' ');
  if (n.nodeType !== 1 || /^(SCRIPT|STYLE|NOSCRIPT|SVG)$/.test(n.tagName)) return '';
  const inner = Array.from(n.childNodes).map(walk).join('');
  const t = n.tagName;
  if (/^H[1-6]$/.test(t)) return '\n\n' + '#'.repeat(+t[1]) + ' ' + inner.trim() + '\n\n';
  if (t === 'A' && n.href) return '[' + inner.trim() + '](' + n.href + ')';
  if (t === 'LI') return '\n- ' + inner.trim();
  if (t === 'P' || t === 'DIV' || t === 'SECTION' || t === 'TR') return '\n' + inner.trim() + '\n';
  if (t === 'TD' || t === 'TH') return inner.trim() + ' | ';
  return inner;
})(document.body || document.documentElement).replace(/\n{3,}/g, '\n\n').trim()"#;

/// `browser_screenshot`: viewport, full page or one element.
pub async fn screenshot(ctx: Ctx, params: Value) -> Result<Value, String> {
    ensure_domains(&ctx).await?;
    let mut request = json!({"format": "png"});
    if let Some(reference) = params.get("ref").and_then(Value::as_str) {
        let node = ctx.tab.borrow().refs.node_for(reference).ok_or_else(|| format!("unknown ref {reference}"))?;
        let _ = ctx.call("DOM.scrollIntoViewIfNeeded", json!({"backendNodeId": node})).await;
        let model = ctx.call("DOM.getBoxModel", json!({"backendNodeId": node})).await?;
        let quad: Vec<f64> = model
            .pointer("/model/border")
            .and_then(Value::as_array)
            .map(|quad| quad.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default();
        if quad.len() != 8 {
            return Err(String::from("that element has no box on the page"));
        }
        request["clip"] = json!({"x": quad[0], "y": quad[1], "width": quad[2] - quad[0], "height": quad[5] - quad[1], "scale": 1});
    } else if params.get("full").and_then(Value::as_bool).unwrap_or(false) {
        request["captureBeyondViewport"] = json!(true);
        let metrics = ctx.call("Page.getLayoutMetrics", json!({})).await?;
        let size = metrics.get("cssContentSize").cloned().unwrap_or(Value::Null);
        request["clip"] = json!({"x": 0, "y": 0, "width": size["width"], "height": size["height"], "scale": 1});
    }
    let answer = ctx.call("Page.captureScreenshot", request).await?;
    let data = answer.get("data").and_then(Value::as_str).unwrap_or_default();
    if data.is_empty() {
        return Err(String::from("the page returned no image"));
    }
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        data.hash(&mut hasher);
        hasher.finish()
    };
    let unchanged = {
        let mut agent = ctx.tab.borrow_mut();
        agent.last_shot.replace(hash) == Some(hash)
    };
    if unchanged && params.get("if_changed").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(json!({"unchanged": true}));
    }
    Ok(json!({"image_base64": data}))
}

/// `browser_wait`: `ms`, `text`, `selector`, or `idle` (no requests in flight).
pub async fn wait(ctx: Ctx, params: Value) -> Result<Value, String> {
    ensure_domains(&ctx).await?;
    let timeout = Duration::from_millis(params.get("timeout_ms").and_then(Value::as_u64).unwrap_or(10_000));
    if let Some(ms) = params.get("ms").and_then(Value::as_u64) {
        ctx.sleep(Duration::from_millis(ms.min(30_000))).await;
        return Ok(json!({"waited": true}));
    }
    if params.get("idle").and_then(Value::as_bool).unwrap_or(false) {
        settle(&ctx).await;
        return Ok(json!({"waited": true}));
    }
    let body = if let Some(text) = params.get("text").and_then(Value::as_str) {
        format!("return (document.body && document.body.innerText || '').includes({})", Value::String(text.to_owned()))
    } else if let Some(selector) = params.get("selector").and_then(Value::as_str) {
        format!("return !!document.querySelector({})", Value::String(selector.to_owned()))
    } else {
        return Err(String::from("browser_wait needs ms, text, selector or idle"));
    };
    let started = ctx.now();
    loop {
        if evaluate(&ctx, &body).await?.as_bool().unwrap_or(false) {
            return Ok(json!({"waited": true}));
        }
        if ctx.now().duration_since(started) > timeout {
            return Err(format!("not there after {} ms", timeout.as_millis()));
        }
        ctx.sleep(Duration::from_millis(200)).await;
    }
}

/// `browser_run`: JS with `page`-side access for 30 s. On a page that isn't this
/// machine's it asks first: a script could do anything an approval guards.
pub async fn run(ctx: Ctx, params: Value) -> Result<Value, String> {
    let code = params.get("code").and_then(Value::as_str).ok_or("browser_run needs code")?.to_owned();
    let url = page_url(&ctx).await?;
    if !policy::is_local(&url) && !ask(&ctx, Reason::Script, &url).await {
        return Ok(policy::stopped_answer(Reason::Script, &Target { name: url, ..Target::default() }));
    }
    let answer = ctx
        .call(
            "Runtime.evaluate",
            json!({"expression": format!("(async () => {{\n{code}\n}})()"), "awaitPromise": true, "returnByValue": true, "userGesture": true, "timeout": 30_000}),
        )
        .await?;
    if let Some(details) = answer.get("exceptionDetails") {
        return Err(details
            .pointer("/exception/description")
            .or_else(|| details.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("the script failed")
            .to_owned());
    }
    let value = answer.pointer("/result/value").cloned().unwrap_or(Value::Null);
    Ok(json!({"result": snapshot::fence(&value.to_string())}))
}

/// `browser_fetch`: a GET with the page's cookies, opening no tab.
pub async fn fetch(ctx: Ctx, params: Value) -> Result<Value, String> {
    let url = params.get("url").and_then(Value::as_str).ok_or("browser_fetch needs url")?;
    let max = params.get("max_chars").and_then(Value::as_u64).unwrap_or(20_000);
    let body = format!(
        "const r = await fetch({}, {{credentials: 'include'}}); const t = await r.text(); return {{status: r.status, type: r.headers.get('content-type'), text: t.slice(0, {max})}}",
        Value::String(url.to_owned())
    );
    let answer = evaluate(&ctx, &body).await?;
    Ok(json!({"status": answer["status"], "type": answer["type"],
        "text": snapshot::fence(answer["text"].as_str().unwrap_or(""))}))
}

fn since_action(params: &Value) -> bool {
    params.get("since").and_then(Value::as_str) == Some("last_action")
}

/// `browser_console`.
pub async fn console(ctx: Ctx, params: Value) -> Result<Value, String> {
    ensure_domains(&ctx).await?;
    let errors_only = params.get("errors_only").and_then(Value::as_bool).unwrap_or(false);
    Ok(ctx.tab.borrow().log.console_json(since_action(&params), errors_only))
}

/// `browser_errors`.
pub async fn errors(ctx: Ctx, params: Value) -> Result<Value, String> {
    ensure_domains(&ctx).await?;
    Ok(ctx.tab.borrow().log.errors_json(since_action(&params)))
}

/// `browser_network`. With `body_of`, one response's body.
pub async fn network(ctx: Ctx, params: Value) -> Result<Value, String> {
    ensure_domains(&ctx).await?;
    if let Some(id) = params.get("body_of").and_then(Value::as_str) {
        let answer = ctx.call("Network.getResponseBody", json!({"requestId": id})).await?;
        let body = answer.get("body").and_then(Value::as_str).unwrap_or("");
        return Ok(json!({"body": snapshot::fence(&body.chars().take(20_000).collect::<String>()),
            "base64": answer.get("base64Encoded")}));
    }
    let failed_only = params.get("failed_only").and_then(Value::as_bool).unwrap_or(false);
    Ok(ctx.tab.borrow().log.network_json(since_action(&params), failed_only))
}

/// `browser_emulate`: viewport or device, dark, offline; `reset` clears them.
pub async fn emulate(ctx: Ctx, params: Value) -> Result<Value, String> {
    if params.get("reset").and_then(Value::as_bool).unwrap_or(false) {
        ctx.call("Emulation.clearDeviceMetricsOverride", json!({})).await?;
        ctx.call("Emulation.setEmulatedMedia", json!({"features": []})).await?;
        ctx.call("Network.emulateNetworkConditions", json!({"offline": false, "latency": 0, "downloadThroughput": -1, "uploadThroughput": -1})).await?;
        return Ok(json!({"reset": true}));
    }
    let device = params.get("device").and_then(Value::as_str);
    let preset = match device {
        Some("phone") => Some((390, 844, 3.0, true)),
        Some("tablet") => Some((820, 1180, 2.0, true)),
        Some("desktop") => Some((1440, 900, 2.0, false)),
        Some(other) => return Err(format!("unknown device {other}: phone, tablet or desktop")),
        None => None,
    };
    let width = params.get("width").and_then(Value::as_i64);
    let height = params.get("height").and_then(Value::as_i64);
    if preset.is_some() || (width.is_some() && height.is_some()) {
        let (w, h, scale, mobile) = preset.unwrap_or((width.unwrap_or(0) as i32, height.unwrap_or(0) as i32, 1.0, false));
        ctx.call(
            "Emulation.setDeviceMetricsOverride",
            json!({"width": w, "height": h, "deviceScaleFactor": scale, "mobile": mobile}),
        )
        .await?;
    }
    if let Some(dark) = params.get("dark").and_then(Value::as_bool) {
        let scheme = if dark { "dark" } else { "light" };
        ctx.call("Emulation.setEmulatedMedia", json!({"features": [{"name": "prefers-color-scheme", "value": scheme}]})).await?;
    }
    if let Some(offline) = params.get("offline").and_then(Value::as_bool) {
        ctx.call("Network.enable", json!({})).await?;
        ctx.call("Network.emulateNetworkConditions", json!({"offline": offline, "latency": 0, "downloadThroughput": -1, "uploadThroughput": -1})).await?;
    }
    Ok(json!({"emulating": true}))
}

/// `browser_ask`: the person does what only they can (login, captcha, OTP, confirm),
/// and this answers when they press Done.
pub async fn ask_tool(ctx: Ctx, params: Value) -> Result<Value, String> {
    let message = params.get("message").and_then(Value::as_str).unwrap_or("Needs you").to_owned();
    let kind = params.get("kind").and_then(Value::as_str).unwrap_or("confirm").to_owned();
    let approval = kind == "confirm";
    let done = ctx.ask_person(PersonAsk { kind, detail: message, approval }).await;
    Ok(json!({"done": done}))
}

/// `browser_upload`: always asks.
pub async fn upload(ctx: Ctx, params: Value) -> Result<Value, String> {
    let reference = params.get("ref").and_then(Value::as_str).ok_or("browser_upload needs ref")?;
    let node = ctx.tab.borrow().refs.node_for(reference).ok_or_else(|| format!("unknown ref {reference}"))?;
    let files: Vec<String> = params
        .get("files")
        .and_then(Value::as_array)
        .map(|files| files.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default();
    if files.is_empty() {
        return Err(String::from("browser_upload needs files"));
    }
    let detail = files.join(", ");
    if !ask(&ctx, Reason::Upload, &detail).await {
        return Ok(policy::stopped_answer(Reason::Upload, &Target { name: detail, ..Target::default() }));
    }
    ctx.call("DOM.setFileInputFiles", json!({"backendNodeId": node, "files": files})).await?;
    Ok(json!({"uploaded": files.len()}))
}

#[cfg(test)]
mod tests {
    use super::super::script::{run as run_job, run_deciding};
    use super::super::{Job, TabAgent, TabHandle};
    use super::*;

    fn tab() -> TabHandle {
        TabHandle::default()
    }

    fn checkout_tree() -> Value {
        json!({"nodes": [
            {"nodeId":"1","role":{"value":"RootWebArea"},"name":{"value":""},"childIds":["2","3"],"backendDOMNodeId":1},
            {"nodeId":"2","parentId":"1","role":{"value":"button"},"name":{"value":"Pay €97"},"childIds":[],"backendDOMNodeId":2},
            {"nodeId":"3","parentId":"1","role":{"value":"button"},"name":{"value":"Next"},"childIds":[],"backendDOMNodeId":3},
        ]})
    }

    /// A page that answers everything an action needs.
    fn page(method: &str, params: &Value) -> Result<Value, String> {
        Ok(match method {
            "Page.getFrameTree" => json!({"frameTree": {"frame": {"id": "main"}}}),
            "Accessibility.getFullAXTree" => checkout_tree(),
            "DOM.getBoxModel" => json!({"model": {"content": [10.0, 10.0, 30.0, 10.0, 30.0, 30.0, 10.0, 30.0]}}),
            "DOM.resolveNode" => {
                json!({"object": {"objectId": format!("node-{}", params["backendNodeId"])}})
            }
            "Runtime.callFunctionOn" => {
                let name = if params["objectId"] == "node-2" { "Pay €97" } else { "Next" };
                json!({"result": {"value": {"name": name}}})
            }
            "Runtime.evaluate" => json!({"result": {"value": {"url": "http://localhost/", "title": "T"}}}),
            _ => json!({}),
        })
    }

    #[test]
    fn a_click_is_a_real_pointer_sequence_at_the_elements_centre() {
        let agent = tab();
        let (_, result) = run_job(Job::new(agent.clone(), |ctx| snapshot(ctx, json!({}))), page);
        assert!(result.unwrap()["snapshot"].as_str().unwrap().contains("[ref=e2]"));
        let (calls, result) = run_job(Job::new(agent, |ctx| act(ctx, json!({"action":"click","ref":"e2"}))), page);
        assert_eq!(result.unwrap()["effect"], "none");
        let pointer: Vec<_> = calls.iter().filter(|(method, _)| method == "Input.dispatchMouseEvent").collect();
        let kinds: Vec<_> = pointer.iter().map(|(_, params)| params["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["mouseMoved", "mousePressed", "mouseReleased"]);
        assert_eq!(pointer[1].1["x"], 20.0);
        assert_eq!(pointer[1].1["y"], 20.0);
    }

    #[test]
    fn pay_stops_for_the_person_and_nothing_is_clicked_when_they_say_no() {
        let agent = tab();
        let _ = run_job(Job::new(agent.clone(), |ctx| snapshot(ctx, json!({}))), page);
        let outcome = run_deciding(Job::new(agent, |ctx| act(ctx, json!({"action":"click","ref":"e1"}))), page, |_| false);
        assert_eq!(outcome.asks.len(), 1);
        assert_eq!(outcome.asks[0].kind, "Pay");
        assert_eq!(outcome.asks[0].detail, "Pay €97");
        let answer = outcome.result.unwrap();
        assert_eq!(answer["stopped"], "needs_approval");
        assert!(!outcome.calls.iter().any(|(method, _)| method == "Input.dispatchMouseEvent"), "not one input event");
    }

    #[test]
    fn pay_goes_through_when_they_allow_it() {
        let agent = tab();
        let _ = run_job(Job::new(agent.clone(), |ctx| snapshot(ctx, json!({}))), page);
        let outcome = run_deciding(Job::new(agent, |ctx| act(ctx, json!({"action":"click","ref":"e1"}))), page, |_| true);
        assert_eq!(outcome.asks.len(), 1);
        assert!(outcome.calls.iter().any(|(method, _)| method == "Input.dispatchMouseEvent"));
    }

    #[test]
    fn an_ordinary_button_asks_nothing() {
        let agent = tab();
        let _ = run_job(Job::new(agent.clone(), |ctx| snapshot(ctx, json!({}))), page);
        let outcome = run_deciding(Job::new(agent, |ctx| act(ctx, json!({"action":"click","ref":"e2"}))), page, |_| false);
        assert!(outcome.asks.is_empty());
    }

    #[test]
    fn typing_in_a_plain_field_asks_nothing_but_a_password_field_does() {
        let agent = tab();
        let _ = run_job(Job::new(agent.clone(), |ctx| snapshot(ctx, json!({}))), page);
        let plain = run_deciding(
            Job::new(agent.clone(), |ctx| act(ctx, json!({"action":"fill","ref":"e2","text":"hi"}))),
            page,
            |_| false,
        );
        assert!(plain.asks.is_empty());
        assert!(plain.calls.iter().any(|(method, params)| method == "Input.insertText" && params["text"] == "hi"));
        let password = |method: &str, params: &Value| {
            if method == "Runtime.callFunctionOn" {
                return Ok(json!({"result": {"value": {"name": "Password", "input_type": "password"}}}));
            }
            page(method, params)
        };
        let stopped = run_deciding(
            Job::new(agent, |ctx| act(ctx, json!({"action":"fill","ref":"e2","text":"secret"}))),
            password,
            |_| false,
        );
        assert_eq!(stopped.asks[0].kind, "Sign in");
        assert!(!stopped.calls.iter().any(|(method, _)| method == "Input.insertText"));
    }

    #[test]
    fn enter_in_a_payment_form_asks_because_it_submits() {
        let agent = tab();
        let form = |method: &str, params: &Value| {
            if method == "Runtime.evaluate" && params["expression"] == "document.activeElement" {
                return Ok(json!({"result": {"objectId": "focused"}}));
            }
            if method == "Runtime.callFunctionOn" {
                return Ok(json!({"result": {"value": {"name": "Coupon", "form_has_card": true}}}));
            }
            page(method, params)
        };
        let outcome = run_deciding(Job::new(agent, |ctx| act(ctx, json!({"action":"press","key":"Enter"}))), form, |_| false);
        assert_eq!(outcome.asks[0].kind, "Pay");
        assert!(!outcome.calls.iter().any(|(method, _)| method == "Input.dispatchKeyEvent"));
    }

    #[test]
    fn a_navigation_before_the_action_is_not_the_actions_effect() {
        let agent = tab();
        let _ = run_job(Job::new(agent.clone(), |ctx| snapshot(ctx, json!({}))), page);
        agent.borrow_mut().log.absorb("Page.frameNavigated", &json!({"frame": {"id": "main"}}));
        let (_, result) = run_job(Job::new(agent, |ctx| act(ctx, json!({"action":"hover","ref":"e2"}))), page);
        assert_eq!(result.unwrap()["navigated"], false);
    }

    #[test]
    fn an_unknown_ref_says_to_take_a_new_snapshot() {
        let (_, result) = run_job(Job::new(tab(), |ctx| act(ctx, json!({"action":"click","ref":"e9"}))), page);
        assert!(result.unwrap_err().contains("new snapshot"));
    }

    #[test]
    fn navigate_waits_for_the_load_event() {
        let agent = tab();
        let log_owner = agent.clone();
        let (calls, result) = run_job(Job::new(agent, |ctx| navigate(ctx, "http://localhost:5173/".into())), move |method, params| {
            if method == "Page.navigate" {
                log_owner.borrow_mut().log.absorb("Page.loadEventFired", &json!({}));
                return Ok(json!({"loaderId": "l1"}));
            }
            page(method, params)
        });
        assert_eq!(result.unwrap()["url"], "http://localhost/");
        assert!(calls.iter().any(|(method, params)| method == "Page.navigate" && params["url"] == "http://localhost:5173/"));
    }

    #[test]
    fn a_failed_navigation_says_why() {
        let answer = |method: &str, params: &Value| {
            if method == "Page.navigate" {
                return Ok(json!({"errorText": "net::ERR_CONNECTION_REFUSED"}));
            }
            page(method, params)
        };
        let (_, result) = run_job(Job::new(tab(), |ctx| navigate(ctx, "http://localhost:1/".into())), answer);
        assert!(result.unwrap_err().contains("ERR_CONNECTION_REFUSED"));
    }

    #[test]
    fn console_and_errors_read_the_tabs_log() {
        let agent = tab();
        agent.borrow_mut().log.absorb("Runtime.consoleAPICalled", &json!({"type":"error","args":[{"value":"boom"}]}));
        let (_, result) = run_job(Job::new(agent, |ctx| console(ctx, json!({}))), page);
        assert_eq!(result.unwrap()["lines"][0]["text"], "boom");
    }

    #[test]
    fn a_script_on_someone_elses_page_asks_and_on_localhost_does_not() {
        let at = |url: &'static str| {
            move |method: &str, params: &Value| {
                if method == "Runtime.evaluate" && params["expression"].as_str().unwrap_or("").contains("location.href") {
                    return Ok(json!({"result": {"value": url}}));
                }
                page(method, params)
            }
        };
        let local = run_deciding(Job::new(tab(), |ctx| run(ctx, json!({"code":"return 1"}))), at("http://localhost:5173/"), |_| false);
        assert!(local.asks.is_empty());
        let remote = run_deciding(Job::new(tab(), |ctx| run(ctx, json!({"code":"return 1"}))), at("https://shop.example/"), |_| false);
        assert_eq!(remote.asks[0].kind, "Run a script");
        assert_eq!(remote.result.unwrap()["stopped"], "needs_approval");
    }

    #[test]
    fn a_screenshot_says_unchanged_only_when_asked_and_only_when_it_is() {
        let agent = tab();
        let image = |method: &str, params: &Value| {
            if method == "Page.captureScreenshot" {
                return Ok(json!({"data": "QUJD"}));
            }
            page(method, params)
        };
        let (_, first) = run_job(Job::new(agent.clone(), |ctx| screenshot(ctx, json!({"if_changed": true}))), image);
        assert_eq!(first.unwrap()["image_base64"], "QUJD");
        let (_, second) = run_job(Job::new(agent.clone(), |ctx| screenshot(ctx, json!({"if_changed": true}))), image);
        assert_eq!(second.unwrap()["unchanged"], true);
        let (_, third) = run_job(Job::new(agent, |ctx| screenshot(ctx, json!({}))), image);
        assert_eq!(third.unwrap()["image_base64"], "QUJD");
    }

    #[test]
    fn the_domains_turn_on_once() {
        let agent = TabHandle::new(std::cell::RefCell::new(TabAgent::default()));
        let (first, _) = run_job(Job::new(agent.clone(), |ctx| console(ctx, json!({}))), page);
        assert_eq!(first.iter().filter(|(method, _)| method.ends_with(".enable")).count(), 6);
        let (second, _) = run_job(Job::new(agent, |ctx| console(ctx, json!({}))), page);
        assert!(second.is_empty());
    }
}

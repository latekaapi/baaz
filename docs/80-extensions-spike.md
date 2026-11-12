# 80 — Extensions spike (12 Nov 2026)

The owner asked what stops extensions in Baaz, and gave permission to download and try one. This doc records what was
tried on the check build and what it showed. Nothing here is built.

## What was done

- Downloaded uBlock Origin Lite 2026.1110.1816 (Manifest V3, 9 MB) from the official `uBlockOrigin/uBOL-home` GitHub
  release into a scratch folder. Nothing in it was executed outside Chromium.
- Ran the check build (its own throwaway profile) with two spike-only library hooks, kept in
  `docs/80-extensions-spike.patch` and **not** in agentic-ui:
  - `AUI_CEF_EXTRA_SWITCHES` passes raw Chromium switches (`load-extension=…`, `remote-debugging-port=9333`), with
    `AUI_CEF_DISABLE_FEATURES=DisableLoadExtensionCommandLineSwitch`.
  - `AUI_CEF_ALLOW_EXTENSIONS=1` lets `chrome-extension://` pages through Baaz's navigation gate.
- The screen was locked, so everything was checked over CDP (Node's WebSocket), not by eye.

## What works (CEF 154, Chrome style)

| Check | Result |
|---|---|
| Extension loads | Its MV3 service worker starts. `chrome://extensions` (developerPrivate) lists it ENABLED / UNPACKED. |
| Network blocking (declarativeNetRequest) | Six rulesets enabled. `https://101com.com/` (only in uBOL's lists, not EasyList) fails with `net::ERR_BLOCKED_BY_CLIENT` from a Baaz page. |
| Extension APIs | `chrome.action`, `chrome.tabs.query` and `declarativeNetRequest` all answer. |
| Its pages | With the gate opened, `popup.html` renders in a Baaz tab. |

## What stops it

1. **Baaz's own gate.** `cef_navigation_allowed` (aui-cef) cancels every `chrome-extension://` page except the PDF
   viewer (`net::ERR_ABORTED`), so popups, options and dashboards never load. It also blocked uBOL's redirect to its
   own stand-in script (`chrome-extension://…/web_accessible_resources/noop.txt` → `ERR_BLOCKED_BY_CLIENT`). Sites
   that rely on those stand-ins (ad-script stubs) can break.
2. **No toolbar.** An extension's button opens its popup *over the active tab*. Baaz draws no Chrome toolbar, so
   there is no button. Opened as a tab instead, uBOL's popup reports on itself ("not a website"), because the tab it
   sees as active is the popup. Baaz needs its own extensions button that opens the popup as a popup tied to the
   page tab, or Chrome's own toolbar on the page view (CEF `chrome_toolbar_type`). Both are unbuilt and unspiked.
3. **The Web Store won't install.** It shows "Install Chrome" instead of "Add to Chrome": Baaz's client hints say
   `Chromium`, not `Google Chrome` (UA `Chrome/154`, brands `Not A(Brand`, `Chromium`). `chrome.webstorePrivate`
   exists, so installing may work if the brand check is satisfied (an unverified guess: spoofing the brand is a policy
   call, and the store's terms apply). Other routes: load unpacked from `chrome://extensions` (developer mode), or a
   Baaz "Add extension…" that installs a downloaded package.
4. **Spaces with their own sign-ins** are separate Chromium profiles (own request contexts). Extensions loaded into
   the main profile are not expected to run there. Not tested.

## Options for the owner

- **A. Leave as is.** Ad blocking is built in (EasyList/EasyPrivacy, plus element hiding since 12 Nov). The footer
  says extensions don't run.
- **B. Minimum useful.** Let extension pages through the gate, add an "Extensions" footer popover listing installed
  ones with a button each that opens its popup over the page, and support "Load unpacked…". Needs a design board and
  a popup-hosting spike (does `chrome.tabs.query({active:true})` see the page tab?).
- **C. Web Store installs** on top of B: needs the brand question settled first.

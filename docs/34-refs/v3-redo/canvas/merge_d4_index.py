#!/usr/bin/env python3
"""Merge the D4 boards and notes into a freshly read copy of the live canvas.json (argv[1]) and write
project/canvas.json. Every other key is kept as read. Run after gen_d4.py, right before publishing."""
import json, os, sys
ROOT = os.path.dirname(os.path.abspath(__file__))
live = json.load(open(sys.argv[1]))
boards = json.load(open(ROOT + "/d4_boards.json"))
for f, e in boards.items():
    if f in live["boards"]:
        live["boards"][f].update({"w": e["w"], "h": e["h"], "title": e["title"]})
    else:
        live["boards"][f] = e
    if f not in live["order"]:
        live["order"].insert(0, f)
OVERWRITE = {"n11", "n10", "n12"}
new_notes = {
 "t6": {"x": 0, "y": 13060, "kind": "title1", "maxW": 2960, "text": "Round 5 (owner, 2026-10-13): the pickers, a nested menu, the effort slider, send / stop emphasis"},
 "n12": {"x": 1520, "y": 13300, "w": 420, "fill": "blue", "text":
        "Round 5. The four composer pickers on the one dropdown (Floating, r 10, the edge + float drop, 6 px padding so the row highlight never touches the panel's edge; rows r 6, selected 9 % + semibold + check): provider rows as mark + name only (another provider on a session with turns opens the hand-off confirm; hand-off also sits in the ⋯ menu), models with context in faint mono and 9 % badges, effort with its nine meanings, approval mode with four. Nested: a submenu opens inside the menu's box 4 px to the right of its row. Effort as a slider: 4 px track, 8 ticks, 16 px thumb, name + meaning above, Default kept as a row. Send / stop: the composer's only solid fill — foreground when a draft, attachment or queued line exists and while running (stop square), muted at Floating when empty. Applied to every screen with a draft or a running turn."},
 "t5": {"x": 0, "y": 12040, "kind": "title1", "maxW": 4400, "text": "Round 4 (owner, 2026-10-13): approval, error and a grilling session, as screens"},
 "n11": {"x": 4560, "y": 12280, "w": 420, "fill": "blue", "text":
        "Round 4. Approval pending as a screen: the warning ring, the command on the Well, the needs-you dot in the sidebar, the status row and stop. Error: the model-error card with its danger ring and one Retry, then the retry status row. Several questions: a wizard, one question at a time in one card. Header: the topic semibold, why in caption, '2 of 4' faint and the auto-resolve tag; a 3 px progress bar under it; then only the current question (header caption, question semibold, its options, Other when allowed). Footer: Back on the left with the keys, then Skip, Explain instead and Next (Continue on the last). Answering advances on its own after a single choice; pick-all waits for Next. The answered ones settle into one quiet row per question once the card closes."},
 "t4": {"x": 0, "y": 9560, "kind": "title1", "maxW": 4400, "text": "Round 3 (owner, 2026-10-13): reading, the composer’s states, the remaining cards"},
 "n10": {"x": 4560, "y": 9800, "w": 420, "fill": "blue", "text":
        "Round 3. Reading: body 14.3 / 23 with 12 between paragraphs, headings with their own air, quote as a 2 px rule, image as a Raised block with its caption inside; content is a new fixture of real project prose (fixtures/msp/synthetic-reading.jsonl). Composer: the chip row for attachments (image thumbnail or file glyph + name + meta, ✕), uploading and failed states, the drop ring, the @ and / menus at Floating, the queued row, running with stop. More cards: plan, summary, search, web, browser, delegated, MCP, your own $ command, skill loaded, markers, needs-you banner, streaming caret and shimmer, the hand-off as a marker row (no card: the confirm dialog already lists what crosses over), unknown kind, the empty session. Everything from the app's captures or the library's own sample data."},
 "t3": {"x": 0, "y": 3800, "kind": "title1", "maxW": 2960,
        "text": "D4 — the transcript on the v3 ladder (2026-10-13): the blocks, the cards, four screens, three homes"},
 "n7": {"x": 3040, "y": 4040, "w": 420, "fill": "blue", "text":
        "D4 rules (proposed, 2026-10-13). Inside the sheet only Raised, Raised-lit and the Well, with the one edge; "
        "blocks and cards take radius 8 (the sheet's own), the user turn keeps 12 like the composer. Rows that only "
        "report — Worked for…, Thought…, Allowed, Answered, the live activity row — carry no surface: a 32 px quiet "
        "row, chevron left, muted text, the 5 % overlay on hover. A card that needs you (approval, question, failure) "
        "swaps the white ring for a 70 % ring in its status colour; nothing else is marked. Shell output and commands "
        "sit on the Well; a diff on Raised with 10 % line tints. Text is the FF roles at the app's 1.1 scale "
        "(body 14.3 / 22, caption 13.2 / 18, title 17.6 / 24, mono 12.65 / 20); spacing is unscaled. "
        "No standing actions anywhere: a hovered turn shows copy (and edit on yours) with its meta; code blocks show copy on hover; tool cards have no buttons; cards carry only the choices the provider minted. Content is lifted from the fixture captures, nothing invented."},
 "n8": {"x": 3040, "y": 6500, "w": 420, "fill": "blue", "text":
        "The four screens, one fixture each: Answer = synthetic-markdown (heading, bullets, inline code, code block, "
        "table, links, the turn's rail and footer, the seam under the composer: a 32 px fade above it, content runs "
        "under). Work = synthetic-toolgroup with the fold open (tool group, thinking row, read group, a resolved "
        "approval row, a failed shell card) and the next turn folded. Question = synthetic-question-error (the ring, "
        "the auto-resolve tag, the status row above the composer, stop in the composer, the needs-you dot in the "
        "sidebar). Goal + tasks = synthetic-todo-goal."},
 "n9": {"x": 3040, "y": 8660, "w": 420, "fill": "blue", "text":
        "Homes for the three controls (owner's placements, docs/40 §3). Toast chip and feedback slot: a corner stack "
        "12 px from the sheet's right, newest toast above the slot, 8 apart; in Sessions the stack sits 8 above the "
        "composer's top edge, in Browse 12 above the sheet's bottom — an overlay window over the native page. Both "
        "are Floating with the edge and the float drop. Downloads: a 28 px icon button in the Browse sidebar's control "
        "row before ✦ and ⋯, a 6 px dot while a download runs, present only while downloads list; its list is the "
        "menu. Open for you: (1) quiet rows instead of cards for Worked / Thought / resolved; (2) radius 8 on blocks; "
        "(3) the status-colour ring on cards that need you; (4) actions only on hover, and only copy / edit; (5) the corner-stack rule."},
}
live.setdefault("notes", {})
for k, v in new_notes.items():
    if k not in live["notes"] or k in OVERWRITE:
        live["notes"][k] = v
# boards: keep the owner's placement of a board that already exists; only size changes come from here
json.dump(live, open(ROOT + "/project/canvas.json", "w"), indent=1, ensure_ascii=False)
print("merged", len(live["boards"]), "boards,", len(live["notes"]), "notes")

//! The composer's own surfaces: the `/`, `@`, model, effort and mode
//! menus, the prompt history the arrows walk, and pasted or attached images.
//!
//! Part of [`SessionView`]; see [`crate::session`] for what
//! the entity owns and why these are its own files.

use super::*;

impl SessionView {
    // ------------------------------------------------------------ the menus

    /// The draft changed: re-derive the caret popovers and step off the
    /// history.
    pub(super) fn on_draft_changed(&mut self, cx: &mut Context<Self>) {
        self.note_draft(cx);
        if self.history.walking() {
            self.history.reset();
        }
        let (draft, caret) = self.draft_and_caret(cx);
        let token = caret_token(&draft, caret);
        self.overlays.update(cx, |overlays, _| {
            let caret_menu = matches!(
                overlays.menu.as_ref().map(|m| m.kind),
                Some(MenuKind::Command) | Some(MenuKind::Mention)
            );
            match token {
                Some((kind, at, filter)) => {
                    let same = overlays.menu.as_ref().is_some_and(|m| m.kind == kind && m.at == at);
                    if same {
                        if let Some(menu) = overlays.menu.as_mut() {
                            menu.filter = filter;
                            menu.selected = 0;
                        }
                    } else {
                        let mut menu = Menu::caret(kind, at);
                        menu.filter = filter;
                        overlays.open(menu);
                    }
                }
                None if caret_menu => overlays.menu = None,
                None => {}
            }
        });
        if let Some(filter) = self
            .overlays
            .read(cx)
            .menu
            .as_ref()
            .filter(|menu| menu.kind == MenuKind::Mention)
            .map(|menu| menu.filter.clone())
        {
            self.refresh_mentions(filter, cx);
        }
        cx.notify();
    }

    pub(super) fn draft_and_caret(&self, cx: &gpui::App) -> (String, usize) {
        let state = self.composer.read(cx);
        (state.value().to_string(), state.cursor())
    }

    /// Open (or close) one of the chip pickers.
    pub fn toggle_picker(&mut self, kind: MenuKind, cx: &mut Context<Self>) {
        let open = self.overlays.read(cx).is_open(kind);
        if open {
            self.overlays.update(cx, |overlays, _| overlays.menu = None);
            cx.notify();
            return;
        }
        let selected = match kind {
            MenuKind::Model => {
                self.load_models(cx);
                self.models.iter().position(|m| m.is_active).unwrap_or(0)
            }
            MenuKind::Effort => EFFORTS.iter().position(|e| *e == self.effort).unwrap_or(0),
            MenuKind::Mode => MODES.iter().position(|m| *m == self.mode()).unwrap_or(0),
            MenuKind::Provider => {
                ProviderId::all().iter().position(|id| *id == self.provider_kind()).unwrap_or(0)
            }
            _ => 0,
        };
        self.overlays.update(cx, |overlays, _| overlays.open(Menu::picker(kind, selected)));
        cx.notify();
    }

    /// How many rows the open menu has, which is what the arrow keys wrap on.
    pub fn menu_rows(&self, cx: &gpui::App) -> usize {
        let overlays = self.overlays.read(cx);
        match overlays.menu.as_ref().map(|m| m.kind) {
            Some(MenuKind::Model) => self.models.len(),
            Some(MenuKind::Effort) => EFFORTS.len(),
            Some(MenuKind::Mode) => MODES.len(),
            Some(MenuKind::Provider) => ProviderId::all().len(),
            Some(MenuKind::Command) => {
                let filter = overlays.menu.as_ref().map(|m| m.filter.clone()).unwrap_or_default();
                let (commands, skill_rows) = self.command_rows(&filter, cx);
                commands.len() + skill_rows.len()
            }
            Some(MenuKind::Mention) => {
                let filter = overlays.menu.as_ref().map(|m| m.filter.clone()).unwrap_or_default();
                self.mention_rows(&filter, cx).len()
            }
            // The shell's own menus are rendered and driven by the window,
            // not by the composer: no rows here, no Enter here.
            Some(MenuKind::Overflow | MenuKind::ViewOptions | MenuKind::Account | MenuKind::Project) => 0,
            None => 0,
        }
    }

    /// Enter on the open menu.
    pub fn confirm_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((kind, selected, filter)) = self
            .overlays
            .read(cx)
            .menu
            .as_ref()
            .map(|m| (m.kind, m.selected, m.filter.clone()))
        else {
            return;
        };
        match kind {
            MenuKind::Model => {
                if let Some(model) = self.models.get(selected).map(|m| m.model_id.clone()) {
                    self.pick_model(&model, cx);
                }
            }
            MenuKind::Effort => {
                if let Some(effort) = EFFORTS.get(selected).copied() {
                    self.pick_effort(effort, cx);
                }
            }
            MenuKind::Mode => {
                if let Some(mode) = MODES.get(selected).copied() {
                    self.pick_mode(mode, cx);
                }
            }
            MenuKind::Provider => {
                if let Some(id) = ProviderId::all().get(selected).copied() {
                    self.pick_provider(id.as_str(), cx);
                }
            }
            MenuKind::Command => {
                let (commands, skill_rows) = self.command_rows(&filter, cx);
                if let Some(command) = commands.get(selected).copied() {
                    self.run_command(command, window, cx);
                } else if let Some(skill) = skill_rows.get(selected.saturating_sub(commands.len())) {
                    let insertion = format!("/{} ", skill.name);
                    self.replace_token(&insertion, window, cx);
                }
            }
            MenuKind::Mention => {
                let rows = self.mention_rows(&filter, cx);
                if let Some(path) = rows.get(selected).cloned() {
                    self.replace_token(&format!("@{path} "), window, cx);
                }
            }
            // Click-driven by the window; Enter stays with the composer.
            MenuKind::Overflow | MenuKind::ViewOptions | MenuKind::Account | MenuKind::Project => {}
        }
    }

    /// A click on a `/` menu row, which names itself rather than its index.
    pub(super) fn select_command(&mut self, id: &SharedString, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(command) = Command::parse(id.as_ref()) {
            self.run_command(command, window, cx);
            return;
        }
        let name = self
            .overlays
            .read(cx)
            .skills_for(&self.workspace)
            .iter()
            .find(|s| s.id == id.as_ref())
            .map(|s| s.name.clone());
        if let Some(name) = name {
            self.replace_token(&format!("/{name} "), window, cx);
        }
    }

    pub(super) fn pick_model(&mut self, model_id: &str, cx: &mut Context<Self>) {
        self.set_model(model_id, cx);
        cx.emit(SessionEvent::ModelSelected { model_id: model_id.to_owned() });
        self.close_menu(cx);
    }

    pub(super) fn pick_effort(&mut self, effort: Option<ReasoningEffort>, cx: &mut Context<Self>) {
        self.effort = effort;
        cx.emit(SessionEvent::EffortSelected { effort: crate::projects::effort_string(effort) });
        self.close_menu(cx);
    }

    /// The session's project display name, for the empty state. Synced by
    /// the window on every activation, so a rename lands without reopening.
    pub(crate) fn set_project_name(&mut self, name: Option<String>) {
        self.project_name = name;
    }

    /// A new session's starting effort, from its project's defaults: applied
    /// before the first turn, reporting nothing back — it already is what
    /// the project says.
    pub(crate) fn set_initial_effort(&mut self, effort: Option<ReasoningEffort>, cx: &mut Context<Self>) {
        self.effort = effort;
        cx.notify();
    }

    pub(super) fn pick_mode(&mut self, mode: PermissionMode, cx: &mut Context<Self>) {
        self.set_mode(mode, cx);
        cx.emit(SessionEvent::ModeSelected { mode: wire_mode(mode) });
        self.close_menu(cx);
    }

    /// A click (or Enter) on a provider menu row, which names the backend's
    /// wire id. Picking the session's own provider just closes the menu —
    /// reopening the lane it already rides is not a swap. A fresh session
    /// swaps through the application (nothing sent, nothing to keep); a
    /// session with turns keeps its lane and the pick starts a new session
    /// on the other backend instead.
    pub(super) fn pick_provider(&mut self, id: &str, cx: &mut Context<Self>) {
        let picked = ProviderId::parse(id);
        if picked == self.provider_kind() {
            self.close_menu(cx);
            return;
        }
        if self.has_turns() {
            cx.emit(SessionEvent::NewSessionOnProvider { provider: picked });
        } else {
            cx.emit(SessionEvent::SwitchProvider { provider: picked });
        }
        self.close_menu(cx);
    }

    pub(super) fn close_menu(&mut self, cx: &mut Context<Self>) {
        self.overlays.update(cx, |overlays, _| overlays.menu = None);
        cx.notify();
    }

    /// Run one client-side slash command (spec §3.10).
    ///
    /// `argument` is whatever followed the command when it was typed; the `/`
    /// menu always passes an empty one, because a menu row carries no text.
    pub fn run_command(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        self.run_command_with(command, String::new(), window, cx);
    }

    /// [`SessionView::run_command`] with whatever followed the command.
    pub(crate) fn run_command_with(&mut self, command: Command, argument: String, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_token("", window, cx);
        match command {
            Command::Model => self.toggle_picker(MenuKind::Model, cx),
            Command::Effort => self.toggle_picker(MenuKind::Effort, cx),
            Command::Mode => self.toggle_picker(MenuKind::Mode, cx),
            Command::Plan => {
                let on = !self.plan;
                self.set_plan(on, cx);
            }
            Command::Compact => self.compact(cx),
            Command::Clear => cx.emit(SessionEvent::NewSession),
            Command::Logout => cx.emit(SessionEvent::Logout),
            Command::Status | Command::Usage => {
                let detail = self.status_text(cx);
                cx.emit(SessionEvent::Status { detail });
            }
            Command::Help => {
                self.overlays.update(cx, |overlays, _| overlays.open(Menu::caret(MenuKind::Command, 0)));
                cx.notify();
            }
            // `/fork` with nothing named opens the turn picker; `/fork <n>`
            // forks the nth newest completed turn with no picker in between.
            Command::Fork => {
                let argument = argument.trim();
                if argument.is_empty() {
                    cx.emit(SessionEvent::ForkPicker);
                } else {
                    match argument.parse::<usize>() {
                        Ok(n) => self.fork_nth(n, cx),
                        Err(_) => self.set_banner("`/fork` takes a turn number, e.g. `/fork 2`.", None, cx),
                    }
                }
            }
            // `/name Fix the parser` renames; `/name` on its own opens the
            // row's field, and `/name ` with nothing after it clears the name.
            Command::Name => {
                let text = argument.trim();
                match (text.is_empty(), argument.is_empty()) {
                    (true, true) => cx.emit(SessionEvent::RenameStart),
                    (true, false) => cx.emit(SessionEvent::Rename { name: None }),
                    _ => cx.emit(SessionEvent::Rename { name: Some(text.to_owned()) }),
                }
            }
            Command::Hide => cx.emit(SessionEvent::Hide),
            Command::Empty => cx.emit(SessionEvent::ToggleEmpty),
            Command::Resume => cx.emit(SessionEvent::Resume),
            Command::Search => cx.emit(SessionEvent::Search),
            Command::Project => cx.emit(SessionEvent::Projects),
            // Window-level commands: the session cannot act on the window, so
            // it hands them up. They reach the same `run_window_command` the
            // ⌘K palette uses, which is what keeps the two routes honest —
            // these six also appear in the composer's own `/` menu (it is
            // built from `Command::ALL`), and a menu row that did nothing
            // would be worse than no row.
            Command::RightBrowser
            | Command::RightDiff
            | Command::RightGit
            | Command::RightFiles
            | Command::Terminal
            | Command::NewTerminalCmd => cx.emit(SessionEvent::WindowCommand(command)),
        }
    }

    /// The `/status` and `/usage` body: everything the session knows about
    /// itself, from the fold rather than from what the app last sent.
    pub(super) fn status_text(&self, cx: &gpui::App) -> String {
        let context = self.context();
        let branch = self.session().and_then(|s| s.branch.clone()).unwrap_or_else(|| "—".to_owned());
        let queued = self.fold.side(&self.session_id).map(|s| s.queued.len()).unwrap_or(0);
        let _ = cx;
        format!(
            "Model: {}\nApproval mode: {}\nReasoning effort: {}\nPlan mode: {}\nContext: {}\nSession tokens: {} prompt · {} output · {} total\nQueued: {queued}\nSession: {}\nWorkspace: {}\nBranch: {branch}",
            self.model(),
            self.mode_label(),
            crate::overlays::effort_label(self.effort),
            if self.plan { "on" } else { "off" },
            context.label(),
            context.prompt_tokens,
            context.output_tokens,
            context.total_tokens,
            self.session_id,
            self.workspace,
        )
    }

    /// Replace the `/…` or `@…` token the caret is in with `insertion`.
    pub(super) fn replace_token(&mut self, insertion: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(at) = self.overlays.read(cx).menu.as_ref().map(|m| m.at) else { return };
        let (draft, caret) = self.draft_and_caret(cx);
        if at > draft.len() || caret > draft.len() || at > caret {
            self.close_menu(cx);
            return;
        }
        let mut next = String::with_capacity(draft.len() + insertion.len());
        next.push_str(&draft[..at]);
        next.push_str(insertion);
        next.push_str(&draft[caret..]);
        let offset = at + insertion.len();
        let position = position_of(&next, offset);
        self.composer.update(cx, |state, cx| {
            state.set_value(next, window, cx);
            state.set_cursor_position(position, window, cx);
        });
        self.note_draft(cx);
        self.close_menu(cx);
    }

    // ---------------------------------------------------------------- history

    /// ↑ on the first line of the draft.
    pub fn history_prev(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.composer.read(cx).value().to_string();
        if let Some(text) = self.history.prev(&draft) {
            self.set_history_value(text, window, cx);
        }
    }

    /// ↓ on the last line of the draft.
    pub fn history_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.history.next() {
            self.set_history_value(text, window, cx);
        }
    }

    /// Whether the caret is on the draft's first (or last) line, which is what
    /// arms ↑ and ↓ for the history rather than for the editor.
    pub fn caret_edges(&self, cx: &gpui::App) -> (bool, bool) {
        let state = self.composer.read(cx);
        let value = state.value();
        let line = state.cursor_position().line;
        let last = value.matches('\n').count() as u32;
        (line == 0, line >= last)
    }

    /// Setting the value fires `InputEvent::Change`, which would reset the
    /// cursor the walk depends on, so the walk's own writes are marked.
    pub(super) fn set_history_value(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let position = position_of(&text, text.len());
        self.composer.update(cx, |state, cx| {
            state.set_value(text, window, cx);
            state.set_cursor_position(position, window, cx);
        });
        self.note_draft(cx);
        cx.notify();
    }

    // ----------------------------------------------------------------- images

    /// ⌘V with an image on the clipboard.
    ///
    /// Returns `false` when the clipboard holds no image, so the caller can let
    /// the textarea's own paste have the keystroke.
    pub fn paste_image(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(item) = cx.read_from_clipboard() else { return false };
        let mut pasted = false;
        for entry in item.into_entries() {
            if let ClipboardEntry::Image(image) = entry {
                self.attach_bytes("pasted", image.bytes.clone(), cx);
                pasted = true;
            }
        }
        pasted
    }

    /// The `+` menu's "Attach file or photo", and the drop of files from
    /// Finder. Image extensions attach as images; everything else is extracted
    /// to text by `attachments` (MSP has no file part to carry the bytes).
    pub fn attach_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        for path in paths {
            let ext =
                path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_lowercase();
            if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp") {
                self.image_seq += 1;
                let id = format!("img-{}", self.image_seq);
                // The chip goes up now; the read, the decode and the
                // thumbnail happen on the background executor and replace it
                // when they land (finding `performance-14`). A ten-megabyte
                // photo used to stall the frame it was dropped on.
                self.images.push(images::placeholder(id.clone(), images::display_name(&path)));
                let work_id = id.clone();
                self.wire_call(
                    cx,
                    move || images::from_path(work_id, &path),
                    move |this: &mut Self, decoded, cx| this.resolve_image(&id, decoded, cx),
                );
                continue;
            }
            if self.files.len() >= attachments::MAX_FILES {
                self.banner = Some(format!(
                    "at most {} files per turn; the rest were not attached",
                    attachments::MAX_FILES
                ));
                continue;
            }
            self.file_seq += 1;
            match attachments::from_path(format!("file-{}", self.file_seq), &path) {
                Ok(file) => self.files.push(file),
                Err(reason) => self.banner = Some(reason),
            }
        }
        cx.notify();
    }

    pub(super) fn attach_bytes(&mut self, name: &str, bytes: Vec<u8>, cx: &mut Context<Self>) {
        self.image_seq += 1;
        let id = format!("img-{}", self.image_seq);
        let chip_name = if name.is_empty() { "pasted image".to_owned() } else { name.to_owned() };
        // Same split as a dropped file (finding `performance-14`): the
        // clipboard already handed over the bytes, but the decode and the
        // thumbnail are the expensive half and they do not belong on a frame.
        self.images.push(images::placeholder(id.clone(), chip_name));
        let (work_id, name) = (id.clone(), name.to_owned());
        self.wire_call(
            cx,
            move || images::from_bytes(work_id, name, &bytes),
            move |this: &mut Self, decoded, cx| this.resolve_image(&id, decoded, cx),
        );
        cx.notify();
    }

    /// A background decode landed: swap the placeholder chip for the image, or
    /// take it away and say why.
    ///
    /// A chip the person removed while the decode ran is simply gone, and the
    /// result is dropped with it — removing a chip means removing it.
    pub(super) fn resolve_image(&mut self, id: &str, decoded: Result<images::Image, String>, cx: &mut Context<Self>) {
        let Some(slot) = self.images.iter().position(|image| image.id == id) else { return };
        match decoded {
            Ok(image) => self.images[slot] = image,
            Err(reason) => {
                self.images.remove(slot);
                self.banner = Some(reason);
            }
        }
        cx.notify();
    }

    /// Whether an attachment is still being read and decoded, which is what
    /// keeps the send button down until it lands (finding `performance-14`).
    pub fn attachments_pending(&self) -> bool {
        self.images.iter().any(|image| image.pending)
    }

    /// Open the system picker for a file. Image extensions attach as images;
    /// everything else is extracted to text, so the prompt accepts any file.
    pub fn prompt_for_image(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let _ = this.update(cx, |this, cx| this.attach_paths(paths, cx));
        }));
    }

    /// Type a menu sigil (`@` or `/`) into the draft so its caret menu opens:
    /// what the `+` menu's Mention and commands rows do. The text goes through
    /// `set_draft`, so the caret lands at the end and the popover opens exactly
    /// as if the person had typed it.
    pub(super) fn insert_sigil(&mut self, sigil: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (draft, _) = self.draft_and_caret(cx);
        let mut next = draft;
        if !next.is_empty() && !next.ends_with(char::is_whitespace) {
            next.push(' ');
        }
        next.push_str(sigil);
        self.set_draft(next, window, cx);
        self.on_draft_changed(cx);
        self.focus_composer(window, cx);
    }
}

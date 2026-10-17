//! Small seams used by Telecat; internal offsets remain UTF-8 bytes.
use super::*;
use gpui::{Context, Window};
use std::ops::Range;
impl EditorState {
    pub fn selection_offsets(&self) -> Vec<(usize, usize)> {
        self.selections
            .iter()
            .map(|s| {
                if s.reversed {
                    (s.end, s.start)
                } else {
                    (s.start, s.end)
                }
            })
            .collect()
    }
    pub fn set_selection_offsets(&mut self, offsets: &[(usize, usize)], cx: &mut Context<Self>) {
        if offsets.is_empty() {
            return;
        }
        self.undo_manager.break_transaction_coalescing();
        let selections = offsets
            .iter()
            .map(|&(a, h)| {
                let a = self.cursor_boundary(a, sum_tree::Bias::Left);
                let h = self.cursor_boundary(h, sum_tree::Bias::Left);
                let mut s =
                    cursor::CursorSelection::new(self.selections.generate_id(), a.min(h), a.max(h));
                s.reversed = h < a;
                s
            })
            .collect();
        self.selections.replace_all(selections);
        self.selections.merge_overlapping();
        self.scroll_to(self.cursor(), None, cx);
        self.pause_blink_cursor(cx);
        cx.notify();
    }
    pub fn edit_batch(
        &mut self,
        edits: &[(Range<usize>, String)],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if edits.is_empty() || !self.is_editable() {
            return;
        }
        let before: Vec<_> = self.selections.iter().copied().collect();
        let mut ordered: Vec<_> = edits.iter().collect();
        ordered.sort_by_key(|(range, _)| range.start);
        let map = |offset: usize| {
            let mut delta: isize = 0;
            for (range, text) in &ordered {
                if offset < range.start {
                    break;
                }
                if offset <= range.end {
                    return range.start.saturating_add_signed(delta) + text.len();
                }
                delta += text.len() as isize - range.len() as isize;
            }
            offset.saturating_add_signed(delta)
        };
        self.undo_manager.break_transaction_coalescing();
        self.replace_text_in_ranges(edits, window, cx);
        let after: Vec<_> = before
            .iter()
            .map(|s| {
                let mut s = *s;
                s.start = map(s.start);
                s.end = map(s.end);
                s.column_anchor = None;
                s
            })
            .collect();
        self.selections.replace_all(after.clone());
        self.selections.merge_overlapping();
        self.undo_manager.record_selections(before, after);
        self.pause_blink_cursor(cx);
    }
    pub fn undo_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.undo(&Undo, window, cx);
    }
    pub fn redo_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.redo(&Redo, window, cx);
    }
    /// Deterministic screenshot caret; cancels its pending timer.
    pub fn freeze_caret(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.blink_cursor.update(cx, |c, cx| c.freeze(visible, cx));
    }
}

//! The code index's side of `App`: Alt+N / Ctrl+Alt+Shift+N symbols from the index, merged
//! with the language server's workspace symbols in the same picker (`lsp::popup::Picker`).

use crate::app::App;
use crate::lsp::popup::{PickItem, PickKind, Popup};

/// Picker rows from index symbols: kind, name, `path:line` (relative to the project root).
pub fn pick_items(app_root: &std::path::Path, hits: &[super::SymbolHit]) -> Vec<PickItem> {
    hits.iter()
        .map(|h| {
            let abs = h.abs();
            let shown = abs.strip_prefix(app_root).map(|r| r.to_string_lossy().into_owned()).unwrap_or_else(|_| abs.display().to_string());
            PickItem {
                kind: h.kind.label(),
                label: h.name.clone(),
                detail: format!("{shown}:{}", h.line + 1),
                path: abs,
                pos: lsp_types::Position { line: h.line, character: h.col },
                depth: 0,
            }
        })
        .collect()
}

impl App {
    /// Starts the index's symbol search for picker request `seq` (an empty query clears it).
    pub fn index_symbols_start(&mut self, query: String, seq: u64) {
        self.lsp.idx_syms = None;
        let q = query.trim().to_string();
        if q.is_empty() {
            if let Some(Popup::Picker(p)) = &mut self.lsp.popup {
                p.idx_items.clear();
                p.merge();
            }
            return;
        }
        if let Some(job) = self.project.code_index.as_ref().and_then(|i| i.find_symbols(None, None, q, 200)) {
            self.lsp.idx_syms = Some((seq, job));
        }
    }

    /// Hands a finished index symbol search to the picker. Returns whether it changed.
    pub fn index_symbols_poll(&mut self) -> bool {
        let Some((seq, job)) = self.lsp.idx_syms.take() else { return false };
        let hits = match job.rx.try_recv() {
            Ok(h) => h,
            Err(_) => {
                if !job.done() {
                    self.lsp.idx_syms = Some((seq, job));
                }
                return false;
            }
        };
        let root = self.project.root.clone();
        let Some(Popup::Picker(p)) = &mut self.lsp.popup else { return false };
        if p.kind != PickKind::Symbols || p.seq != seq {
            return false;
        }
        p.idx_items = pick_items(&root, &hits);
        p.merge();
        true
    }
}

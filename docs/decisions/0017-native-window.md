# 0017: Native window next to the terminal version

Date: 2026-11-01. Status: first version on branch `native-window`.

## Context
astra asked for a stand-alone editor that doesn't need a terminal ("can we make this to a stand alone editor then so we dont
need to run this in terminal"), and said yes to the recommended option of three: the same interface in a native window, rather than
a full GUI rewrite or a terminal bundled in an app. Decision 0001 says terminal editor; this adds to it and doesn't replace it.

## Decision
- **Second way to run, same code:** `catlas --window` and `Code Atlas.app` run the same `App` and `ui::draw`. Only a ratatui `Backend` and the
  event source differ. The terminal version stays, and the e2e suite keeps testing it.
- **winit + softbuffer + fontdue:** a CPU painter on a retained 0RGB buffer, repainting only cells ratatui reports as changed. No GPU
  stack to debug, and a frame can be written to a PNG without a window, which is how the look gets checked here.
- **Fonts from the system:** Menlo on a Mac, `fc-match monospace` on Linux, no bundled font; `[window] font` overrides it. Fallbacks load on
  first need. Box, block and braille characters are drawn by hand so they join without gaps.
- **Keys become the crossterm events a kitty-protocol terminal sends,** so handlers and hint tables don't change.
- **Cmd works as Ctrl** for every chord, except in the terminal pane, where it must never turn into Ctrl+C. Ctrl keeps working.
- **Left Option is Alt, right Option types.** The IDEA chords need Alt, and German layouts need Option for `@ [ ] { }`.
- **winit's default menu is off:** its Cmd+Q would quit without asking about unsaved files. Cmd+Q and the close button go through
  `App::close_requested`, from any focus.
- **No terminal means no shell setup:** take `PATH` from the login shell (3 s limit) and open the newest recent workspace, else `~/Projects`.

## Consequences
- Still a monospaced grid: no smooth pixel scrolling or mixed font sizes. A full GUI stays possible later.
- No menu bar yet, so no Cmd+H or menu items. No icon. Dead keys commit through `Ime::Commit`, but their preview isn't drawn.
- The window has no e2e suite; `--render-png` and unit tests cover the painter and key mapping, and real input needs a person.
- `control-center` has its own decision 0016 and runtime themes: the painter's default colours must use `theme::active()` when both are merged.

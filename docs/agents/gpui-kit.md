# gpui-kit 0.8 gotchas

Behaviour of the pinned `gpui-kit =0.8.1` (gpui-component / gpui-base 0.8.1)
that only reading its source reveals. Verified against gpui-component 0.8.0
and gpui-base 0.8.0; the table header labels against 0.8.1.

## Windows

- Open application windows with `gpui_kit::open_window(options, cx, build)`
  (clippy enforces it, see `clippy.toml`): it wraps the view in `base::Root`, whose
  gpui-component plugin (`WindowState`, registered by `gpui_kit::init`)
  supplies the theme's background, text colour and font, and hosts
  tooltips. Without it, unpainted regions render black text on a black
  window (#52). `build` returns the view, not another Root.

- Start the app with `astrolabe::application()` (clippy enforces it): it
  registers the icon assets that `gpui_kit::application()` leaves out.
  Without them every `IconName` icon draws nothing while its button still
  takes clicks, which UI tests cannot see (#51).

Reference: `crates/app/src/main.rs`.

## DockArea panels

- Add panels with `dock.add_panel_view(panel_handle(entity), placement, …)`
  (`gpui_kit::component::dock::panel_handle`). `DockArea::add_panel` is the
  base-layer call: it wraps the entity in base's handle, so `DockSkin`'s tab
  bar cannot recover the gpui-component `Panel` and tab names come out empty.
- Implement `Panel::title` on every panel. The default renders
  `"Unnamed"`; `tab_name` alone does not title the tab.
- Select an already-open tab with `dock.select_panel(PanelId::from(entity.entity_id()), …)`;
  `dock.panel(id).is_some()` tells whether the user has closed it since.
- Per-tab close buttons are off until `DockSkin::set_close_button_visible(true, …)`
  on the skin `DockSkin::dock_area` returns (#51).
- Neither `add_panel_view` nor `select_panel` moves focus. Focus the
  panel yourself, and have each panel `.track_focus(&self.focus_handle)`
  on its root: focus left in a hidden tab is outside the dispatch tree,
  so dock actions (`ClosePanel` on `cmd-w`) reach nothing (#51).

Reference: `crates/app/src/session_view.rs`.

## Tables (`TableState` / `DataTable`)

- `TableState` caches column widths in `col_groups` and re-reads
  `delegate.column()` only on `refresh()`. A delegate whose widths depend
  on the panel width calls `refresh` when that width changes (#68).
- `DataTable` is `bordered` by default (`border_1`): the columns' viewport
  is the element's width minus 2px.
- Cells render as `w(column width).overflow_hidden().whitespace_nowrap()`.
  A header cell's id is `("col-header", ix)`, and since 0.8.1 it carries
  the column name as its accessible label. A `render_th` that labels its
  own content duplicates it, and `try_find_by_label(name)` panics as
  ambiguous (#73).

Reference: `crates/app/src/resource_list.rs`.

## Command palette (`Command` / `CommandState`)

- `on_confirm` and `on_cancel` run deferred (`window.defer`): the callback
  gets the `window` and a plain `&mut App`, with no current window.
  `WeakEntity::update_in(cx, ..)` returns `Err("entity has no current
  window")` there, and a trailing `.ok()` hid it: Enter and a row click did
  nothing and Esc never closed the palette. Call
  `view.update(cx, |this, cx| ..)` and pass the callback's `window` into
  the method (#317).
- An `Err` from `WeakEntity::update` means only that the entity is gone,
  which a deferred callback can meet when its tab closes in the same
  frame. Ignore it with `.ok()` and a comment saying so; `.expect(..)`
  turns a harmless race into a crash (#317 review).
- A palette row has no label, only `Role::ListBoxOption` and an id that is
  its `IndexPath`. A test finds rows with
  `window.find_all_by_role(Role::ListBoxOption)` (rows in order) and
  clicks with `simulate_mouse_down` / `simulate_mouse_up` at the row's
  `bounds().center()`; `window.click` needs an id (#317).
- A test that confirms a command counts what the command does (a
  scripted Cluster's Precheck closure), not that the palette closed: on
  screen the editor's automatic Precheck masks the manual one (#317).

Reference: `crates/app/tests/yaml_editor.rs`.

## UI tests (`gpui_kit::test`)

- `try_find_by_label` panics on a label that matches more than one element
  ("ambiguous accessible label"). Scope the query to a container id:
  `window.within("detail-metadata").try_find_by_label(…)`. A change that
  adds a second element carrying an existing label breaks the old tests
  that look it up: #276's Namespaces `list-stale` banner repeated the
  workspace's `namespace-stale` text, and `workspace.rs` panicked. Only the
  full-workspace run showed it; find by name (`try_find("list-stale")`) and
  assert each banner on its own.
- A label lookup only sees elements that carry both `.aria_label(…)` and
  `.test_support()`, and `.aria_label` needs an element with an `.id(…)`.
- `.test_support()` wraps the element in `Observed<…>`
  (`gpui_kit::base::test_support::Observed`): a helper that returns a
  labeled element declares that type, not `Stateful<Div>` (#17).
- A layer drawn over other elements (menu, popover) needs `.occlude()`;
  without it, `window.click` on the layer also lands on whatever sits
  beneath it. When a click causes an unrelated state change (e.g. the
  dock switches tabs), suspect a missing `.occlude()` first. The
  namespace menu over the dock tab bar is the reference case (#41).

- `cx.debug_bounds(selector)` returns one element: the last one drawn
  under that selector. Index-free selectors such as
  `dock-tab-close-button` therefore name only the last tab's button, so an
  assertion that a button is absent from an earlier tab proves nothing.
  Assert on the element the selector does return (e.g. it lies inside
  `dock-tab-{n}`), or on its absence once a single candidate is left (#51).

- Opening a built-in kind's list:
  `support::open_list(ui, BuiltinKind, watch, objects)`; the list then a
  row's detail: `support::open_detail(ui, BuiltinKind, watch, objects,
  name)`; a `DynamicObject` from JSON: `support::object` (#101, #117).
  A Secret's `data` values must be base64: the list drops a row it cannot
  decode, and the test then fails on a missing `row-…` id, not on the
  fixture (#106).
- Back to an already open detail tab: `support::reopen_detail(ui,
  BuiltinKind, name)` clicks the kind's nav item and the row, and the row
  click selects the open tab. A local copy of those clicks in a test file
  is a `CODING_STANDARDS.md` finding (#306).
- `ScriptedCluster`'s `logs` and `exec` closures return a fixed list, and
  the stream or session ends as soon as it is sent: every Logs or Exec tab
  in a UI test is already finished (`Stream failed`, `Exited`). For a
  still-live one, build the window with `support::open_streaming`: its
  streams stay open (`Streaming…`, `Session running`) until
  `ui.open_streams.close()`. It is the way to test a branch for a live
  stream, such as "reopening does not restart it" (#332).
- An element's accessible label, as the frame draws it:
  `ui.label(id)` (`crates/app/tests/support`); it panics when `id` is
  not rendered (#160). A test that builds its window without
  `support::open` calls `support::label(cx, id)` (#161).
- A Workspace over several scripted Clusters:
  `support::open_workspace(cx, clusters)`; `support::open` builds its
  one Cluster on it (#161).
- The report of a plain Apply that `kubectl` refuses with a 409 on
  `metadata.name`, which offers Force Apply:
  `support::conflict_report(snapshot)`; a Precheck's that notes the
  unknown field `spec.bogus`: `support::unknown_field_report(snapshot)`
  (#161). A Global error on a document is a rule of `EditLoop`: test it
  there with a plain `#[test]` (#299).
- The line tints of the YAML editor are not elements, so `try_find`
  cannot see them. Build the view over a scripted Session
  (`Session::scripted`, `YamlEditorView::with_session`), press the
  Apply keystroke and read `YamlEditorView::apply_mark_ranges`
  (`a_failed_apply_marks_its_line`, #299).
- Clicking an element whose handler listens for mouse events, such as a
  dock tab or its close button, which `window.click` does not reach:
  `support::mouse_click(cx, id)` presses and releases on where `id` was
  last drawn, so render a frame first (#106).
- `Ui::frame` (`crates/app/tests/support`) renders the whole window
  before it queries, so every view shows fresh state whether or not it
  notified. To prove a view redraws on its own (a timer, a
  subscription), query the last drawn frame with
  `ui.cx.update(|window, _| …)`. A notify from any view redraws the whole
  window, so pick data under which the window's other views stay quiet
  for the interval tested (#70).
- Time: panels read `astrolabe::clock::now(cx)`; clippy rejects
  `jiff::Timestamp::now`. `support::follow_test_clock` drives that clock
  from the test scheduler, so `cx.executor().advance_clock` ages objects
  (`crates/app/tests/age_ticks.rs`) (#70).

- Render order: compare `window.try_find(id).bounds().origin.y` across
  ids. `try_find` takes an `&'static str`, a `String`, or a tuple id such
  as `("col-header", 6usize)` (the index must be `usize`), and the returned
  `ElementSnapshot` has no `id()`. In the cargo registry, the snapshot API
  is in `gpui-base`'s `src/test_support.rs`, and the window queries
  (`find`, `find_all`, `within`) are in `gpui-kit`'s `src/test.rs` (#20,
  #89).
- Only `.test_support()` elements have snapshots: `find` on a plain
  `.id(…)` container (a summary-table row) panics "missing ElementId",
  though `within` still scopes to it (#89).
- A "missing ElementId" panic names the id and scope in its first line,
  then dumps every registered path (thousands of tokens). Read the first
  line (`grep -A3 'stdout ----' | cut -c1-300`); to see what is
  registered, grep the dump for the missing id's neighbours (#89).
- A view test cannot click an element after the state that renders it is
  gone: `run_until_parked` redraws the window, and the click panics
  "missing ElementId". To cover a click racing a change (the object left
  its watch before Delete was confirmed), test the seam the handler reads
  (`DetailTarget::object`) without a view (#96).
- `simulate_keystrokes` and `dispatch_action` run until parked, so the
  scripted answer lands before the test's next step. To act while a
  request is in flight (select another Cluster before the Apply's answer
  arrives), call `window.dispatch_keystroke` and the next action in one
  `cx.update`, then `run_until_parked`: `press_then_select` in
  `crates/app/tests/workspace_yaml.rs` (#161).
- Layout in a flex row: a child shrinks below its text only with
  `.min_w_0()`; `.flex_1()` gives it the width left over and `.flex_none()`
  keeps a button whole. `.when(..)` needs `use
  gpui_kit::prelude::FluentBuilder as _` and goes before `.test_support()`
  (`Observed` has no `when`); `.map(|el| if c { .. } else { .. })` also works
  (#213).
- A test that proves an element stays inside the window calls
  `ui.cx.simulate_resize(size(px(w), px(1600.)))` before opening the
  detail, and compares `bounds().origin.x + size.width` with `px(w)`. At
  800pt high, `window.click("pick-port-…")` panicked "is not visible"
  (#213).
- A test that loops over several states and panics on the first failure
  proves only the first. Give each state its own test, then break that
  state's branch and watch the test fail. A `sed` that rustfmt's line
  breaks defeat changes nothing and the test "survives": check the diff
  before trusting the run (#213).

- Snapshots carry element bounds, never laid-out text: a text element's
  bounds are its box, whatever the text's width or truncation. An
  ellipsis, a wrap, or text overflowing its box shows only in the manual
  check's screenshot; a UI test asserts the box the text is laid out in
  (#75).
- Detail ids (`crates/app/src/resource_detail.rs`): a summary table is
  `detail-table-<title>`, a status field `field-<label>`, and every
  condition row `field-Condition`, whatever its type. A summary table's
  rows are `table-<title>-head` and `table-<title>-row-<n>`, its header
  cells `table-<title>-head-<col>`, and its body cells
  `table-<title>-cell-<row>-<col>`, unique within the table (#89).
  A summary table builds cells with `table_cell`, whose text element is
  `<cell id>-text` (#75).
  The related objects under a detail (a Pod's Events, a Deployment's
  Pods) are `detail-related`, drawn by `render_related` with the list's
  `RowsTable` in `related-table`: body cells `related-cell-<row>-<col>`,
  each row's first cell `related-row-<key>`, header cells the table's own
  `("col-header", <col>)`, and the table's cell around a body cell
  `table-cell-<row>:<col>`. A new table fits its columns one frame after
  it first paints, so a test renders one more `ui.frame` before measuring
  it (#98).

- `use gpui_kit::*` exports gpui's `test` attribute macro. A `#[test]`
  in a module that glob-imports it (directly or through `use super::*`)
  fails with "recursion limit reached while expanding `#[test]`". In such
  a module, import what the tests use by name (`spikes/datatable/src/auto.rs`)
  (#78).

Reference: `crates/app/tests/crd_views.rs`.

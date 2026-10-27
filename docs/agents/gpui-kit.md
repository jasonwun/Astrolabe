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

## UI tests (`gpui_kit::test`)

- `try_find_by_label` panics on a label that matches more than one element
  ("ambiguous accessible label"). Scope the query to a container id:
  `window.within("detail-metadata").try_find_by_label(…)`.
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

- Render order: compare `window.try_find(id).bounds().origin.y` across
  ids. `try_find` takes an `&'static str`, a `String`, or a tuple id such
  as `("col-header", 6usize)` (the index must be `usize`), and the returned
  `ElementSnapshot` has no `id()`; its API is in `gpui-base`'s
  `src/test_support.rs` in the cargo registry (#20).
- Detail ids: a summary table is `detail-table-<title>`, a status field
  `field-<label>`, and every condition row `field-Condition`, whatever
  its type (`crates/app/src/resource_detail.rs`).

Reference: `crates/app/tests/crd_views.rs`.

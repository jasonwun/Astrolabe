# gpui-kit 0.8 gotchas

Behaviour of the pinned `gpui-kit =0.8.0` (gpui-component / gpui-base 0.8.0)
that only reading its source reveals. Verified against gpui-component 0.8.0
and gpui-base 0.8.0.

## DockArea panels

- Add panels with `dock.add_panel_view(panel_handle(entity), placement, …)`
  (`gpui_kit::component::dock::panel_handle`). `DockArea::add_panel` is the
  base-layer call: it wraps the entity in base's handle, so `DockSkin`'s tab
  bar cannot recover the gpui-component `Panel` and tab names come out empty.
- Implement `Panel::title` on every panel. The default renders
  `"Unnamed"`; `tab_name` alone does not title the tab.
- Select an already-open tab with `dock.select_panel(PanelId::from(entity.entity_id()), …)`;
  `dock.panel(id).is_some()` tells whether the user has closed it since.

Reference: `crates/app/src/session_view.rs`.

## UI tests (`gpui_kit::test`)

- `try_find_by_label` panics on a label that matches more than one element
  ("ambiguous accessible label"). Scope the query to a container id:
  `window.within("detail-metadata").try_find_by_label(…)`.
- A label lookup only sees elements that carry both `.aria_label(…)` and
  `.test_support()`, and `.aria_label` needs an element with an `.id(…)`.

Reference: `crates/app/tests/crd_views.rs`.

Origin: #16 session, all hit mid-implementation.

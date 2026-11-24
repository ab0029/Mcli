use rune::Icon;

const fn outline(name: &'static str, body: &'static str) -> Icon {
    Icon {
        name,
        view: 24.,
        body,
    }
}

pub const FOLDER: Icon = outline(
    "mcli-project-folder",
    r#"<g fill="none" stroke="currentColor" stroke-width="1.7" stroke-linejoin="round"><path d="M2.5 7V5.5A1.5 1.5 0 0 1 4 4h5l2.4 2.8H20a1.5 1.5 0 0 1 1.5 1.5V18a2 2 0 0 1-2 2h-15a2 2 0 0 1-2-2Z"/><path d="M3 9.5h18"/></g>"#,
);
pub const SETTINGS: Icon = outline(
    "mcli-settings",
    r#"<g fill="none" stroke="currentColor" stroke-width="1.7" stroke-linejoin="round"><path d="m10 2-.6 2.2-1.6.9-2.2-.6-2 3.5 1.6 1.6v2L3.6 13l2 3.6 2.2-.6 1.6 1L10 19.2h4l.6-2.2 1.6-1 2.2.6 2-3.6-1.6-1.4v-2L20.4 8l-2-3.5-2.2.6-1.6-.9L14 2Z" transform="translate(0 1.4)"/><circle cx="12" cy="12" r="3.3"/></g>"#,
);
pub const SEARCH: Icon = outline(
    "mcli-search",
    r#"<g fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><circle cx="10.2" cy="10.2" r="6.5"/><path d="m15 15 5.3 5.3"/></g>"#,
);
pub const DOWN: Icon = outline(
    "mcli-chevron-down",
    r#"<path d="m5 9 7 7 7-7" fill="none" stroke="currentColor" stroke-width="2.3" stroke-linecap="round" stroke-linejoin="round"/>"#,
);
pub const RIGHT: Icon = outline(
    "mcli-chevron-right",
    r#"<path d="m9 5 7 7-7 7" fill="none" stroke="currentColor" stroke-width="2.3" stroke-linecap="round" stroke-linejoin="round"/>"#,
);
pub const HISTORY: Icon = outline(
    "mcli-history",
    r#"<g fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M3 10a9 9 0 1 1 .8 6M3 5v5h5m4-4v6l4 2"/></g>"#,
);
pub const PROMPTS: Icon = outline(
    "mcli-prompts",
    r#"<g fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d="M4 4h16v12H9l-5 4z"/><path d="M8 8h8m-8 4h5"/></g>"#,
);
pub const READY: Icon = outline(
    "mcli-ready",
    r#"<circle cx="12" cy="12" r="8.8" fill="none" stroke="currentColor" stroke-width="2"/><path d="m7.8 12 3 3.1 5.4-6.2" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/>"#,
);
pub const WAIT: Icon = outline(
    "mcli-wait",
    r#"<g fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M5.5 3.5h13m-13 17h13M7 4c0 4 2 5.7 5 8-3 2.3-5 4-5 8m10-16c0 4-2 5.7-5 8 3 2.3 5 4 5 8"/></g>"#,
);
pub const ERROR: Icon = outline(
    "mcli-error",
    r#"<path d="m12 2.5 10 18H2z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"/><path d="M12 8v5m0 2.5v1" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>"#,
);
pub const PAUSE: Icon = outline(
    "mcli-interrupted",
    r#"<g fill="none" stroke="currentColor" stroke-width="1.8"><circle cx="12" cy="12" r="9"/><path d="M9.5 7.5v9m5-9v9"/></g>"#,
);
pub const EXIT: Icon = outline(
    "mcli-exit",
    r#"<g fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" stroke-linecap="round"><path d="M11 4H4v16h7m-2-8h12m-5-5 5 5-5 5"/></g>"#,
);
pub const ONLINE: Icon = outline(
    "mcli-online",
    r#"<circle cx="12" cy="12" r="7.8" fill="none" stroke="currentColor" stroke-width="2.2"/>"#,
);
pub const KIMI: Icon = outline(
    "mcli-kimi",
    r#"<path d="M5.5 4.5v15M6.2 12l8.5-7.5M10.4 10.6 18.5 19.5" fill="none" stroke="currentColor" stroke-width="2.3" stroke-linecap="round" stroke-linejoin="round"/><circle cx="18.2" cy="5.8" r="1.7" fill="currentColor"/>"#,
);
pub const GROK: Icon = outline(
    "mcli-grok",
    r#"<path d="M19.2 6.8A8.6 8.6 0 1 0 20.4 13" fill="none" stroke="currentColor" stroke-width="2.1" stroke-linecap="round"/><path d="M5.4 18.6 18.8 5.2" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round"/>"#,
);

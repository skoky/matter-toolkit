use ratatui::{
    prelude::*,
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::app::App;
use crate::state::AppState;
use crate::types::{ActionDialog, EndpointSummary, FocusPane, ManageState, Modal, Screen, StatusKind};
use crate::utils::{first_socket_addr, format_device_types};

// ─── Top-level draw ──────────────────────────────────────────────────────────

pub fn draw(frame: &mut Frame<'_>, app: &App) {
    match &app.screen {
        Screen::Overview => draw_overview(frame, app),
        Screen::Manage(manage) => draw_manage(frame, app, manage),
    }

    if let Some(modal) = &app.modal {
        draw_modal(frame, modal);
    }
}

// ─── Overview screen ─────────────────────────────────────────────────────────

fn draw_overview(frame: &mut Frame<'_>, app: &App) {
    let areas = Layout::vertical([
        Constraint::Length(4), // header: title + session info
        Constraint::Min(10),   // device lists
        Constraint::Length(6), // saved devices
        Constraint::Length(4), // footer: status + keybindings
    ])
    .split(frame.area());

    // ── Header ──
    let header = Paragraph::new(vec![
        Line::from(Span::styled(
            " Matter Client",
            Style::new().bold().fg(Color::Cyan),
        )),
        Line::from(vec![
            Span::raw("  Controller: "),
            Span::styled(
                format!("{}", app.state.controller_id),
                Style::new().fg(Color::Yellow),
            ),
            Span::raw("   Fabric: "),
            Span::styled(app.state.fabric_label.clone(), Style::new().fg(Color::Yellow)),
        ]),
    ])
    .block(Block::new().borders(Borders::ALL).border_type(BorderType::Rounded));
    frame.render_widget(header, areas[0]);

    // ── Device lists ──
    let body = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(areas[1]);

    render_commissionable_list(frame, app, body[0]);
    render_commissioned_list(frame, app, body[1]);

    // ── Saved devices ──
    let saved = Paragraph::new(saved_devices_text(&app.state))
        .block(
            Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(" Saved Devices "),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(saved, areas[2]);

    // ── Footer ──
    frame.render_widget(overview_footer(app), areas[3]);
}

fn render_commissionable_list(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .commissionable
        .iter()
        .map(|d| {
            ListItem::new(vec![
                Line::from(Span::styled(d.display_name.clone(), Style::new().bold())),
                Line::from(vec![
                    Span::styled(d.device_type.clone(), Style::new().fg(Color::Cyan).dim()),
                    Span::raw("  "),
                    Span::styled(
                        first_socket_addr(&d.addresses, d.port),
                        Style::new().dim(),
                    ),
                ]),
                Line::from(Span::styled(
                    format!(
                        "disc={}  vid={}  pid={}",
                        d.discriminator.as_deref().unwrap_or("-"),
                        d.vendor_id.as_deref().unwrap_or("-"),
                        d.product_id.as_deref().unwrap_or("-")
                    ),
                    Style::new().dim(),
                )),
            ])
        })
        .collect();

    let mut state = list_state(app.selected_commissionable, items.is_empty());
    let list = List::new(items)
        .block(focus_block(" Commissionable ", app.focus == FocusPane::Commissionable))
        .highlight_style(Style::new().fg(Color::Black).bg(Color::Cyan).bold())
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_commissioned_list(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .commissioned
        .iter()
        .map(|d| {
            let (name, managed_span) = if let Some(known) = &d.known {
                (
                    known.label.clone(),
                    Span::styled(
                        format!(" node={}", known.node_id),
                        Style::new().fg(Color::Green).dim(),
                    ),
                )
            } else {
                (
                    d.display_name.clone(),
                    Span::styled(" (unmanaged)", Style::new().fg(Color::DarkGray).dim()),
                )
            };

            let mut lines = vec![
                Line::from(vec![Span::styled(name, Style::new().bold()), managed_span]),
                Line::from(Span::styled(
                    first_socket_addr(&d.addresses, d.port),
                    Style::new().dim(),
                )),
            ];
            if d.known.is_some() && d.display_name != lines[0].to_string() {
                lines.push(Line::from(Span::styled(
                    format!("svc: {}", d.display_name),
                    Style::new().dim(),
                )));
            }
            ListItem::new(lines)
        })
        .collect();

    let mut state = list_state(app.selected_commissioned, items.is_empty());
    let list = List::new(items)
        .block(focus_block(" Commissioned ", app.focus == FocusPane::Commissioned))
        .highlight_style(Style::new().fg(Color::Black).bg(Color::Green).bold())
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, area, &mut state);
}

fn overview_footer(app: &App) -> Paragraph<'static> {
    Paragraph::new(vec![
        Line::from(Span::styled(app.status.clone(), status_style(app.status_kind))),
        Line::from(Span::styled(
            "Tab/←/→ switch pane  ↑↓ select  r refresh  c commission  Enter/m manage  q quit",
            Style::new().dim(),
        )),
    ])
    .block(
        Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded),
    )
}

// ─── Manage screen ───────────────────────────────────────────────────────────

fn draw_manage(frame: &mut Frame<'_>, app: &App, manage: &ManageState) {
    let areas = Layout::vertical([
        Constraint::Length(4), // device header
        Constraint::Min(10),   // endpoints + details
        Constraint::Length(4), // footer
    ])
    .split(frame.area());

    // ── Header ──
    let header = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(" ", Style::new()),
            Span::styled(manage.device.label.clone(), Style::new().bold().fg(Color::Yellow)),
        ]),
        Line::from(vec![
            Span::raw("  node="),
            Span::styled(
                format!("{}", manage.device.node_id),
                Style::new().fg(Color::Cyan),
            ),
            Span::raw("   addr="),
            Span::styled(manage.device.last_address.clone(), Style::new().fg(Color::Cyan)),
        ]),
    ])
    .block(
        Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Managed Device "),
    );
    frame.render_widget(header, areas[0]);

    // ── Body: endpoint list + details pane ──
    let body = Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(areas[1]);

    render_endpoint_list(frame, manage, body[0]);
    render_endpoint_details(frame, manage, body[1]);

    // ── Footer ──
    frame.render_widget(manage_footer(app), areas[2]);
}

fn render_endpoint_list(frame: &mut Frame<'_>, manage: &ManageState, area: Rect) {
    let items: Vec<ListItem> = manage
        .endpoints
        .iter()
        .map(|ep| {
            let label = ep
                .label
                .clone()
                .unwrap_or_else(|| "unnamed".to_string());
            let caps = endpoint_capability_spans(ep);
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(format!("ep{} ", ep.id), Style::new().dim()),
                    Span::styled(label, Style::new().bold()),
                ]),
                Line::from(caps),
            ])
        })
        .collect();

    let mut state = list_state(manage.selected_endpoint, items.is_empty());
    let list = List::new(items)
        .block(
            Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(" Endpoints "),
        )
        .highlight_style(Style::new().fg(Color::Black).bg(Color::Yellow).bold())
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, area, &mut state);
}

fn endpoint_capability_spans(ep: &EndpointSummary) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    if ep.has_on_off {
        spans.push(Span::styled("[on/off]", Style::new().fg(Color::Yellow).dim()));
        spans.push(Span::raw(" "));
    }
    if !ep.actions.is_empty() {
        spans.push(Span::styled("[actions]", Style::new().fg(Color::Magenta).dim()));
        spans.push(Span::raw(" "));
    }
    if spans.is_empty() {
        spans.push(Span::styled("no capabilities", Style::new().dim()));
    }
    spans
}

fn render_endpoint_details(frame: &mut Frame<'_>, manage: &ManageState, area: Rect) {
    let text = selected_endpoint_text(manage);
    let details = Paragraph::new(text)
        .block(
            Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(" Details "),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(details, area);
}

fn selected_endpoint_text(manage: &ManageState) -> Text<'static> {
    let Some(ep) = manage.endpoints.get(manage.selected_endpoint) else {
        return Text::from(Line::from(Span::styled(
            "No endpoints found.",
            Style::new().dim(),
        )));
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled("Endpoint:  ", Style::new().dim()),
            Span::styled(format!("{}", ep.id), Style::new().bold()),
        ]),
        Line::from(vec![
            Span::styled("Label:     ", Style::new().dim()),
            Span::styled(
                ep.label.clone().unwrap_or_else(|| "unnamed".to_string()),
                Style::new().bold(),
            ),
        ]),
        Line::from(vec![
            Span::styled("Types:     ", Style::new().dim()),
            Span::raw(format_device_types(&ep.device_types)),
        ]),
        Line::from(vec![
            Span::styled("OnOff:     ", Style::new().dim()),
            Span::styled(
                if ep.has_on_off { "yes" } else { "no" },
                if ep.has_on_off {
                    Style::new().fg(Color::Yellow)
                } else {
                    Style::new().dim()
                },
            ),
        ]),
    ];

    if !ep.actions.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("Actions:", Style::new().dim())));
        for action in &ep.actions {
            lines.push(Line::from(Span::styled(
                format!(
                    "  {} (id={})",
                    action.name.clone().unwrap_or_else(|| "unnamed".to_string()),
                    action.action_id.unwrap_or_default()
                ),
                Style::new(),
            )));
        }
    }

    Text::from(lines)
}

fn manage_footer(app: &App) -> Paragraph<'static> {
    Paragraph::new(vec![
        Line::from(Span::styled(app.status.clone(), status_style(app.status_kind))),
        Line::from(Span::styled(
            "↑↓ select  o on  p off  a actions  n rename  f fabric  d decommission  b/Esc back",
            Style::new().dim(),
        )),
    ])
    .block(
        Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded),
    )
}

// ─── Modals ──────────────────────────────────────────────────────────────────

fn draw_modal(frame: &mut Frame<'_>, modal: &Modal) {
    let area = centered_rect(68, 40, frame.area());
    frame.render_widget(Clear, area);

    match modal {
        Modal::Message(msg) => {
            let paragraph = Paragraph::new(vec![
                Line::from(msg.as_str()),
                Line::from(""),
                Line::from(Span::styled("Press Enter or Esc to dismiss.", Style::new().dim())),
            ])
            .block(
                Block::new()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .title(" Working… "),
            )
            .wrap(Wrap { trim: true });
            frame.render_widget(paragraph, area);
        }
        Modal::Confirm(dialog) => {
            let paragraph = Paragraph::new(vec![
                Line::from(dialog.message.as_str()),
                Line::from(""),
                Line::from(Span::styled("y = yes    n / Esc = cancel", Style::new().dim())),
            ])
            .block(
                Block::new()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(Color::Red))
                    .title(format!(" {} ", dialog.title)),
            )
            .wrap(Wrap { trim: true });
            frame.render_widget(paragraph, area);
        }
        Modal::Input(dialog) => {
            draw_input_box(frame, area, &dialog.title, &dialog.value, &dialog.help);
        }
        Modal::Action(dialog) => {
            draw_action_dialog(frame, area, dialog);
        }
        Modal::CommissionDeviceName { pending, value } => {
            draw_input_box(
                frame,
                area,
                "Commission — Device Name",
                value,
                &format!(
                    "Give this device a local name.  Suggested: {}",
                    pending.device_label
                ),
            );
        }
        Modal::CommissionFabricName { pending, value } => {
            draw_input_box(
                frame,
                area,
                "Commission — Fabric Label",
                value,
                &format!(
                    "Label written to node {}. Default: {}",
                    pending.node_id, pending.fabric_label
                ),
            );
        }
        Modal::CommissionEndpointName { pending, index, value } => {
            let ep = &pending.endpoints[*index];
            let step = format!("{}/{}", index + 1, pending.endpoints.len());
            draw_input_box(
                frame,
                area,
                &format!("Commission — Endpoint {} Label  [{step}]", ep.id),
                value,
                "Local alias for this endpoint.  Press Enter to accept.",
            );
        }
    }
}

fn draw_input_box(frame: &mut Frame<'_>, area: Rect, title: &str, value: &str, help: &str) {
    let paragraph = Paragraph::new(vec![
        Line::from(Span::styled(help, Style::new().dim())),
        Line::from(""),
        Line::from(vec![
            Span::styled("> ", Style::new().fg(Color::Cyan)),
            Span::styled(value, Style::new().bold()),
            Span::styled("█", Style::new().fg(Color::Cyan)),
        ]),
        Line::from(""),
        Line::from(Span::styled("Enter = confirm    Esc = cancel", Style::new().dim())),
    ])
    .block(
        Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(Color::Cyan))
            .title(format!(" {title} ")),
    )
    .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}

fn draw_action_dialog(frame: &mut Frame<'_>, area: Rect, dialog: &ActionDialog) {
    let items: Vec<ListItem> = dialog
        .options
        .iter()
        .map(|opt| ListItem::new(opt.label.clone()))
        .collect();
    let mut state = list_state(dialog.selected, items.is_empty());
    let list = List::new(items)
        .block(
            Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(Color::Magenta))
                .title(format!(" {} ", dialog.title))
                .title_bottom(Line::from(Span::styled(
                    " ↑↓ select   Enter invoke   Esc cancel ",
                    Style::new().dim(),
                ))),
        )
        .highlight_style(Style::new().fg(Color::Black).bg(Color::Magenta).bold())
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, area, &mut state);
}

// ─── Saved devices summary ───────────────────────────────────────────────────

fn saved_devices_text(state: &AppState) -> Text<'static> {
    if state.devices.is_empty() {
        return Text::from(Span::styled(
            "No saved devices.",
            Style::new().dim(),
        ));
    }

    let mut lines = Vec::new();
    for device in &state.devices {
        lines.push(Line::from(vec![
            Span::styled(device.label.clone(), Style::new().bold()),
            Span::styled(
                format!("  node={}  {}", device.node_id, device.last_address),
                Style::new().dim(),
            ),
        ]));
        let aliases: Vec<String> = state
            .endpoint_aliases
            .iter()
            .filter(|a| a.node_id == device.node_id)
            .map(|a| format!("ep{}={}", a.endpoint_id, a.label))
            .collect();
        if !aliases.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("  {}", aliases.join("  ")),
                Style::new().dim(),
            )));
        }
    }
    Text::from(lines)
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn focus_block(title: &str, focused: bool) -> Block<'_> {
    if focused {
        Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(Color::Cyan))
            .title(Span::styled(title, Style::new().fg(Color::Cyan).bold()))
    } else {
        Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(Color::DarkGray))
            .title(Span::styled(title, Style::new().fg(Color::DarkGray)))
    }
}

fn status_style(kind: StatusKind) -> Style {
    match kind {
        StatusKind::Success => Style::new().fg(Color::Green),
        StatusKind::Progress => Style::new().fg(Color::Yellow),
        StatusKind::Error => Style::new().fg(Color::Red),
        StatusKind::Normal => Style::new(),
    }
}

fn list_state(selected: usize, empty: bool) -> ListState {
    let mut state = ListState::default();
    if !empty {
        state.select(Some(selected));
    }
    state
}

fn centered_rect(percent_x: u16, percent_y: u16, rect: Rect) -> Rect {
    let popup = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(rect);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(popup[1])[1]
}


//! Kitchen printing to network thermal printers.
//!
//! A printer is server-local hardware configuration. When a round is sent, the
//! order projection enqueues one print job per printer. A background worker
//! renders each job as ESC/POS bytes, sends it over TCP (usually port 9100),
//! and retries transient failures. The ticket never includes prices.

use std::collections::BTreeMap;
use std::time::Duration;

use nexo_core::{Operation, Uuid};
use serde::{Deserialize, Serialize};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::watch;

use crate::error::AppError;
use crate::time::{format_local_time, now_ms};

/// Default raw TCP port used by network thermal printers.
pub const DEFAULT_PORT: i64 = 9100;
/// Maximum delivery attempts before a job is marked failed.
pub const MAX_ATTEMPTS: i64 = 8;

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const RETRY_BASE_MS: i64 = 5_000;

/// Ticket data stored with a print job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub kind: String,
    pub business_name: String,
    pub printer_name: String,
    pub order_label: String,
    pub order_code: String,
    pub order_type: String,
    pub round_number: Option<i64>,
    pub sent_at_ms: i64,
    pub sent_by: String,
    pub reprint: bool,
    pub fallback: bool,
    pub lines: Vec<TicketLine>,
}

/// One line on a kitchen ticket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketLine {
    pub quantity: i64,
    pub name: String,
    pub notes: String,
}

#[derive(Debug, Clone)]
struct Printer {
    id: String,
    name: String,
}

#[derive(Debug, Clone)]
struct OrderContext {
    business_id: String,
    business_name: String,
    order_label: String,
    order_code: String,
    order_type: String,
}

#[derive(Debug, Clone)]
struct RoundLine {
    quantity: i64,
    name: String,
    notes: String,
    printer_id: Option<String>,
}

#[derive(Debug)]
struct PrintGroup {
    printer_index: usize,
    fallback: bool,
    lines: Vec<TicketLine>,
}

/// Enqueues one print job per printer for a round that was just sent.
///
/// This runs inside the order-sent projection transaction, so a committed send
/// always has its print jobs. Lines whose category has no active printer use
/// the first active printer as a fallback; if there are no active printers the
/// send succeeds without printing.
pub async fn enqueue_round(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &Operation,
    order_id: &str,
    round_number: i64,
    sent_at_ms: i64,
) -> Result<(), AppError> {
    let Some(context) = load_order_context(transaction, order_id).await? else {
        return Ok(());
    };
    let printers = active_printers(transaction, &context.business_id).await?;
    if printers.is_empty() {
        tracing::debug!(order = %order_id, "no active printers; skipping kitchen ticket");
        return Ok(());
    }

    let lines = load_round_lines(transaction, order_id, round_number).await?;
    if lines.is_empty() {
        return Ok(());
    }

    let sent_by = resolve_user_name(
        transaction,
        operation
            .payload
            .get("sent_by")
            .and_then(serde_json::Value::as_str),
    )
    .await?;
    let groups = group_lines(lines, &printers);
    let dedupe_key = format!("order_sent:{}:{}", operation.id, round_number);

    for group in groups.values() {
        let printer = &printers[group.printer_index];
        let ticket = Ticket {
            kind: "kitchen".to_owned(),
            business_name: context.business_name.clone(),
            printer_name: printer.name.clone(),
            order_label: context.order_label.clone(),
            order_code: context.order_code.clone(),
            order_type: context.order_type.clone(),
            round_number: Some(round_number),
            sent_at_ms,
            sent_by: sent_by.clone(),
            reprint: false,
            fallback: group.fallback,
            lines: group.lines.clone(),
        };
        insert_job(
            transaction,
            &context.business_id,
            printer,
            Some(order_id),
            Some(round_number),
            "kitchen",
            &dedupe_key,
            &ticket,
            sent_at_ms,
        )
        .await?;
    }

    Ok(())
}

/// Enqueues a test ticket for one printer.
pub async fn enqueue_test(
    transaction: &mut Transaction<'_, Sqlite>,
    business_id: &str,
    printer_id: &str,
    business_name: &str,
) -> Result<String, AppError> {
    let row = sqlx::query(
        "SELECT id, name
         FROM printer
         WHERE id = ?1 AND business_id = ?2",
    )
    .bind(printer_id)
    .bind(business_id)
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(AppError::NotFound)?;
    let printer = printer_from_row(&row)?;
    let now = now_ms();
    let ticket = Ticket {
        kind: "test".to_owned(),
        business_name: business_name.to_owned(),
        printer_name: printer.name.clone(),
        order_label: String::new(),
        order_code: String::new(),
        order_type: String::new(),
        round_number: None,
        sent_at_ms: now,
        sent_by: String::new(),
        reprint: false,
        fallback: false,
        lines: Vec::new(),
    };
    let dedupe_key = format!("test:{}", Uuid::now_v7());

    insert_job(
        transaction,
        business_id,
        &printer,
        None,
        None,
        "test",
        &dedupe_key,
        &ticket,
        now,
    )
    .await
}

/// Enqueues new print jobs for an already sent round. Returns the job count.
pub async fn enqueue_reprint(
    transaction: &mut Transaction<'_, Sqlite>,
    business_id: &str,
    order_id: &str,
    round_number: i64,
    actor_name: &str,
) -> Result<usize, AppError> {
    let Some(context) = load_order_context(transaction, order_id).await? else {
        return Ok(0);
    };
    if context.business_id != business_id {
        return Ok(0);
    }

    let printers = active_printers(transaction, business_id).await?;
    if printers.is_empty() {
        return Ok(0);
    }

    let lines = load_round_lines(transaction, order_id, round_number).await?;
    if lines.is_empty() {
        return Ok(0);
    }

    let groups = group_lines(lines, &printers);
    let dedupe_key = format!("reprint:{}", Uuid::now_v7());
    let now = now_ms();

    for group in groups.values() {
        let printer = &printers[group.printer_index];
        let ticket = Ticket {
            kind: "kitchen".to_owned(),
            business_name: context.business_name.clone(),
            printer_name: printer.name.clone(),
            order_label: context.order_label.clone(),
            order_code: context.order_code.clone(),
            order_type: context.order_type.clone(),
            round_number: Some(round_number),
            sent_at_ms: now,
            sent_by: actor_name.to_owned(),
            reprint: true,
            fallback: group.fallback,
            lines: group.lines.clone(),
        };
        insert_job(
            transaction,
            business_id,
            printer,
            Some(order_id),
            Some(round_number),
            "kitchen",
            &dedupe_key,
            &ticket,
            now,
        )
        .await?;
    }

    Ok(groups.len())
}

async fn load_order_context(
    transaction: &mut Transaction<'_, Sqlite>,
    order_id: &str,
) -> Result<Option<OrderContext>, AppError> {
    let row = sqlx::query(
        "SELECT o.business_id, o.order_type, o.display_code, o.customer_name,
                COALESCE(t.name, '') AS table_name,
                b.name AS business_name
         FROM restaurant_order o
         JOIN business b ON b.id = o.business_id
         LEFT JOIN dining_table t ON t.id = o.table_id
         WHERE o.id = ?1",
    )
    .bind(order_id)
    .fetch_optional(&mut **transaction)
    .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    let order_type: String = row.try_get("order_type")?;
    let display_code: String = row.try_get("display_code")?;
    let table_name: String = row.try_get("table_name")?;
    let customer_name: Option<String> = row.try_get("customer_name")?;
    let order_label = match order_type.as_str() {
        "dine_in" if !table_name.is_empty() => table_name,
        _ => customer_name
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| display_code.clone()),
    };

    Ok(Some(OrderContext {
        business_id: row.try_get("business_id")?,
        business_name: row.try_get("business_name")?,
        order_label,
        order_code: display_code,
        order_type,
    }))
}

async fn active_printers(
    transaction: &mut Transaction<'_, Sqlite>,
    business_id: &str,
) -> Result<Vec<Printer>, AppError> {
    let rows = sqlx::query(
        "SELECT id, name
         FROM printer
         WHERE business_id = ?1 AND is_active = 1
         ORDER BY created_at_ms, id",
    )
    .bind(business_id)
    .fetch_all(&mut **transaction)
    .await?;

    rows.iter().map(printer_from_row).collect()
}

fn printer_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Printer, AppError> {
    Ok(Printer {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
    })
}

async fn load_round_lines(
    transaction: &mut Transaction<'_, Sqlite>,
    order_id: &str,
    round_number: i64,
) -> Result<Vec<RoundLine>, AppError> {
    let rows = sqlx::query(
        "SELECT ol.description, ol.quantity, COALESCE(ol.notes, '') AS notes,
                c.printer_id
         FROM order_line ol
         LEFT JOIN menu_item mi ON mi.id = ol.menu_item_id
         LEFT JOIN menu_category c ON c.id = mi.category_id
         WHERE ol.order_id = ?1
           AND ol.round_number = ?2
           AND ol.status IN ('sent', 'ready', 'served')
         ORDER BY ol.created_at_ms, ol.id",
    )
    .bind(order_id)
    .bind(round_number)
    .fetch_all(&mut **transaction)
    .await?;

    rows.iter()
        .map(|row| {
            Ok(RoundLine {
                quantity: row.try_get("quantity")?,
                name: row.try_get("description")?,
                notes: row.try_get("notes")?,
                printer_id: row.try_get("printer_id")?,
            })
        })
        .collect()
}

async fn resolve_user_name(
    transaction: &mut Transaction<'_, Sqlite>,
    user_id: Option<&str>,
) -> Result<String, AppError> {
    let Some(user_id) = user_id else {
        return Ok(String::new());
    };

    Ok(
        sqlx::query_scalar("SELECT display_name FROM app_user WHERE id = ?1")
            .bind(user_id)
            .fetch_optional(&mut **transaction)
            .await?
            .unwrap_or_default(),
    )
}

fn group_lines(lines: Vec<RoundLine>, printers: &[Printer]) -> BTreeMap<String, PrintGroup> {
    let mut groups: BTreeMap<String, PrintGroup> = BTreeMap::new();

    for line in lines {
        let mapped_index = line
            .printer_id
            .as_deref()
            .and_then(|printer_id| printers.iter().position(|p| p.id == printer_id));
        let printer_index = mapped_index.unwrap_or(0);
        let printer = &printers[printer_index];
        let group = groups
            .entry(printer.id.clone())
            .or_insert_with(|| PrintGroup {
                printer_index,
                fallback: false,
                lines: Vec::new(),
            });
        if mapped_index.is_none() {
            group.fallback = true;
        }
        group.lines.push(TicketLine {
            quantity: line.quantity,
            name: line.name,
            notes: line.notes,
        });
    }

    groups
}

#[allow(clippy::too_many_arguments)]
async fn insert_job(
    transaction: &mut Transaction<'_, Sqlite>,
    business_id: &str,
    printer: &Printer,
    order_id: Option<&str>,
    round_number: Option<i64>,
    kind: &str,
    dedupe_key: &str,
    ticket: &Ticket,
    created_at_ms: i64,
) -> Result<String, AppError> {
    let job_id = Uuid::now_v7().to_string();
    let payload = serde_json::to_string(ticket)?;

    sqlx::query(
        "INSERT INTO print_job (
            id, business_id, printer_id, order_id, round_number, kind,
            dedupe_key, payload, status, attempts, created_at_ms, available_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', 0, ?9, 0)
         ON CONFLICT (dedupe_key, printer_id) DO NOTHING",
    )
    .bind(&job_id)
    .bind(business_id)
    .bind(&printer.id)
    .bind(order_id)
    .bind(round_number)
    .bind(kind)
    .bind(dedupe_key)
    .bind(payload)
    .bind(created_at_ms)
    .execute(&mut **transaction)
    .await?;

    Ok(job_id)
}

/// Renders one ticket as ESC/POS bytes.
pub fn render_ticket(ticket: &Ticket, paper_width_mm: i64) -> Vec<u8> {
    let columns = if paper_width_mm <= 58 { 32 } else { 48 };
    let mut out = Vec::new();

    // Initialize, select CP850 (Spanish accents), and center the header.
    out.extend_from_slice(&[0x1B, 0x40]);
    out.extend_from_slice(&[0x1B, 0x74, 0x02]);
    out.extend_from_slice(&[0x1B, 0x61, 0x01]);
    push_wrapped(&mut out, &ticket.business_name, columns, true);
    push_wrapped(&mut out, &ticket.printer_name, columns, false);

    out.extend_from_slice(&[0x1D, 0x21, 0x11]);
    let title = if ticket.kind == "test" {
        "PRUEBA"
    } else if ticket.reprint {
        "REIMPRESION"
    } else {
        "COMANDA"
    };
    push_line(&mut out, title);
    out.extend_from_slice(&[0x1D, 0x21, 0x00]);
    out.extend_from_slice(&[0x1B, 0x61, 0x00]);
    push_separator(&mut out, columns, b'=');

    if ticket.kind == "test" {
        push_wrapped(&mut out, "Impresora de prueba", columns, false);
        push_line(&mut out, "");
        push_wrapped(
            &mut out,
            "Si puedes leer esto, la impresora esta conectada.",
            columns,
            false,
        );
    } else {
        let mut heading = ticket.order_label.clone();
        if !ticket.order_code.is_empty() {
            if !heading.is_empty() {
                heading.push_str(" - ");
            }
            heading.push_str(&ticket.order_code);
        }
        push_wrapped(&mut out, &heading, columns, true);

        let mut subheading = match ticket.order_type.as_str() {
            "dine_in" => "Salon".to_owned(),
            "takeout" => "Para llevar".to_owned(),
            _ => String::new(),
        };
        if let Some(round_number) = ticket.round_number {
            if !subheading.is_empty() {
                subheading.push_str(" - ");
            }
            subheading.push_str(&format!("Ronda {round_number}"));
        }
        push_wrapped(&mut out, &subheading, columns, false);

        let mut meta = format!("Hora {}", short_time(ticket.sent_at_ms));
        if !ticket.sent_by.is_empty() {
            meta.push_str(" - ");
            meta.push_str(&ticket.sent_by);
        }
        push_wrapped(&mut out, &meta, columns, false);
        push_separator(&mut out, columns, b'-');

        for line in &ticket.lines {
            push_item(&mut out, line, columns);
        }
        push_separator(&mut out, columns, b'-');

        if ticket.fallback {
            push_wrapped(&mut out, "SIN CATEGORIA", columns, true);
            push_wrapped(
                &mut out,
                "Revisar la categoria del plato en el menu.",
                columns,
                false,
            );
        }
    }

    // Feed and cut. Printers without a cutter ignore the command.
    out.extend_from_slice(&[0x1B, 0x64, 0x03]);
    out.extend_from_slice(&[0x1D, 0x56, 0x42, 0x00]);
    out
}

fn push_item(out: &mut Vec<u8>, line: &TicketLine, columns: usize) {
    let prefix = format!("{}x ", line.quantity);
    let prefix_width = prefix.chars().count();
    let name_width = columns.saturating_sub(prefix_width).max(1);
    let parts = wrap(&line.name, name_width);

    for (index, part) in parts.iter().enumerate() {
        if index == 0 {
            push_line(out, &format!("{prefix}{part}"));
        } else {
            push_line(out, &format!("{}{part}", " ".repeat(prefix_width)));
        }
    }

    let notes = line.notes.trim();
    if !notes.is_empty() {
        let notes_width = columns.saturating_sub(2).max(1);
        for part in wrap(notes, notes_width) {
            push_line(out, &format!("  {part}"));
        }
    }
}

fn push_wrapped(out: &mut Vec<u8>, text: &str, columns: usize, bold: bool) {
    if bold {
        out.extend_from_slice(&[0x1B, 0x45, 0x01]);
    }
    for part in wrap(text, columns) {
        push_line(out, &part);
    }
    if bold {
        out.extend_from_slice(&[0x1B, 0x45, 0x00]);
    }
}

fn push_separator(out: &mut Vec<u8>, columns: usize, byte: u8) {
    out.extend(std::iter::repeat_n(byte, columns));
    out.push(b'\n');
}

fn push_line(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(&encode_cp850(text));
    out.push(b'\n');
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        wrap_paragraph(paragraph, width.max(1), &mut out);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn wrap_paragraph(text: &str, width: usize, out: &mut Vec<String>) {
    let mut current = String::new();

    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        if word_len > width {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            let mut chunk = String::new();
            let mut chunk_len = 0;
            for ch in word.chars() {
                chunk.push(ch);
                chunk_len += 1;
                if chunk_len == width {
                    out.push(std::mem::take(&mut chunk));
                    chunk_len = 0;
                }
            }
            if !chunk.is_empty() {
                out.push(chunk);
            }
            continue;
        }

        if current.is_empty() {
            current.push_str(word);
        } else if current.chars().count() + 1 + word_len <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            out.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }

    if !current.is_empty() {
        out.push(current);
    }
}

fn short_time(timestamp_ms: i64) -> String {
    format_local_time(timestamp_ms).chars().take(5).collect()
}

fn encode_cp850(text: &str) -> Vec<u8> {
    text.chars().map(cp850_byte).collect()
}

fn cp850_byte(ch: char) -> u8 {
    match ch {
        '\n' => b'\n',
        '\t' | '\r' => b' ',
        ' '..='~' => ch as u8,
        '\u{00A0}' => b' ',
        'Ç' => 0x80,
        'ü' => 0x81,
        'é' => 0x82,
        'â' => 0x83,
        'ä' => 0x84,
        'à' => 0x85,
        'å' => 0x86,
        'ç' => 0x87,
        'ê' => 0x88,
        'ë' => 0x89,
        'è' => 0x8A,
        'ï' => 0x8B,
        'î' => 0x8C,
        'ì' => 0x8D,
        'Ä' => 0x8E,
        'Å' => 0x8F,
        'É' => 0x90,
        'æ' => 0x91,
        'Æ' => 0x92,
        'ô' => 0x93,
        'ö' => 0x94,
        'ò' => 0x95,
        'û' => 0x96,
        'ù' => 0x97,
        'ÿ' => 0x98,
        'Ö' => 0x99,
        'Ü' => 0x9A,
        'ø' => 0x9B,
        '£' => 0x9C,
        'Ø' => 0x9D,
        '×' => 0x9E,
        'ƒ' => 0x9F,
        'á' => 0xA0,
        'í' => 0xA1,
        'ó' => 0xA2,
        'ú' => 0xA3,
        'ñ' => 0xA4,
        'Ñ' => 0xA5,
        'ª' => 0xA6,
        'º' => 0xA7,
        '¿' => 0xA8,
        '®' => 0xA9,
        '¬' => 0xAA,
        '½' => 0xAB,
        '¼' => 0xAC,
        '¡' => 0xAD,
        '«' => 0xAE,
        '»' => 0xAF,
        'Á' => 0xB5,
        'Â' => 0xB6,
        'À' => 0xB7,
        '©' => 0xB8,
        '¢' => 0xBD,
        '¥' => 0xBE,
        'ã' => 0xC6,
        'Ã' => 0xC7,
        'ð' => 0xD0,
        'Ð' => 0xD1,
        'Ê' => 0xD2,
        'Ë' => 0xD3,
        'È' => 0xD4,
        'Í' => 0xD6,
        'Î' => 0xD7,
        'Ï' => 0xD8,
        'Ó' => 0xE0,
        'ß' => 0xE1,
        'Ô' => 0xE2,
        'Ò' => 0xE3,
        'õ' => 0xE4,
        'Õ' => 0xE5,
        'µ' => 0xE6,
        'þ' => 0xE7,
        'Þ' => 0xE8,
        'Ú' => 0xE9,
        'Û' => 0xEA,
        'Ù' => 0xEB,
        'ý' => 0xEC,
        'Ý' => 0xED,
        '°' => 0xF8,
        '·' => 0xFA,
        '–' | '—' => b'-',
        '‘' | '’' => b'\'',
        '“' | '”' => b'"',
        '…' => b'.',
        '€' => b'E',
        _ => b'?',
    }
}

/// Processes at most one pending print job. Returns whether a job was claimed.
pub async fn process_next(pool: &SqlitePool) -> Result<bool, AppError> {
    let now = now_ms();
    let mut transaction = pool.begin().await?;
    let row = sqlx::query(
        "SELECT id, printer_id, payload, attempts
         FROM print_job
         WHERE status = 'pending' AND available_at_ms <= ?1
         ORDER BY created_at_ms, id
         LIMIT 1",
    )
    .bind(now)
    .fetch_optional(&mut *transaction)
    .await?;

    let Some(row) = row else {
        transaction.commit().await?;
        return Ok(false);
    };

    let job_id: String = row.try_get("id")?;
    let printer_id: String = row.try_get("printer_id")?;
    let payload: String = row.try_get("payload")?;
    let attempts: i64 = row.try_get("attempts")?;

    let claimed = sqlx::query(
        "UPDATE print_job
         SET status = 'printing', claimed_at_ms = ?2, attempts = attempts + 1
         WHERE id = ?1 AND status = 'pending'",
    )
    .bind(&job_id)
    .bind(now)
    .execute(&mut *transaction)
    .await?;

    if claimed.rows_affected() == 0 {
        transaction.rollback().await?;
        return Ok(false);
    }
    transaction.commit().await?;

    let attempt = attempts + 1;
    let printer_row = sqlx::query(
        "SELECT name, host, port, paper_width_mm
         FROM printer
         WHERE id = ?1",
    )
    .bind(&printer_id)
    .fetch_optional(pool)
    .await?;
    let Some(printer_row) = printer_row else {
        fail_job(pool, &job_id, attempt, "the printer no longer exists", true).await?;
        return Ok(true);
    };

    let printer_name: String = printer_row.try_get("name")?;
    let host: String = printer_row.try_get("host")?;
    let port: i64 = printer_row.try_get("port")?;
    let paper_width_mm: i64 = printer_row.try_get("paper_width_mm")?;

    let ticket: Ticket = match serde_json::from_str(&payload) {
        Ok(ticket) => ticket,
        Err(error) => {
            fail_job(
                pool,
                &job_id,
                attempt,
                &format!("invalid ticket payload: {error}"),
                true,
            )
            .await?;
            return Ok(true);
        }
    };

    let bytes = render_ticket(&ticket, paper_width_mm);
    match send_bytes(&host, port, &bytes).await {
        Ok(()) => {
            sqlx::query(
                "UPDATE print_job
                 SET status = 'printed', printed_at_ms = ?2, last_error = NULL
                 WHERE id = ?1",
            )
            .bind(&job_id)
            .bind(now_ms())
            .execute(pool)
            .await?;
            tracing::info!(
                job = %job_id,
                printer = %printer_name,
                "printed kitchen ticket"
            );
        }
        Err(error) => {
            fail_job(pool, &job_id, attempt, &error, false).await?;
        }
    }

    Ok(true)
}

async fn send_bytes(host: &str, port: i64, bytes: &[u8]) -> Result<(), String> {
    let port = u16::try_from(port).map_err(|_| format!("invalid printer port: {port}"))?;
    let address = format!("{host}:{port}");
    let mut stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
        .await
        .map_err(|_| format!("connection to {address} timed out"))?
        .map_err(|error| format!("could not connect to {address}: {error}"))?;
    let _ = stream.set_nodelay(true);

    tokio::time::timeout(WRITE_TIMEOUT, stream.write_all(bytes))
        .await
        .map_err(|_| format!("writing to {address} timed out"))?
        .map_err(|error| format!("could not write to {address}: {error}"))?;
    tokio::time::timeout(WRITE_TIMEOUT, stream.flush())
        .await
        .map_err(|_| format!("flushing {address} timed out"))?
        .map_err(|error| format!("could not flush {address}: {error}"))?;
    let _ = stream.shutdown().await;

    Ok(())
}

async fn fail_job(
    pool: &SqlitePool,
    job_id: &str,
    attempt: i64,
    error: &str,
    permanent: bool,
) -> Result<(), AppError> {
    let error: String = error.chars().take(500).collect();

    if permanent || attempt >= MAX_ATTEMPTS {
        sqlx::query(
            "UPDATE print_job
             SET status = 'failed', last_error = ?2, claimed_at_ms = NULL
             WHERE id = ?1",
        )
        .bind(job_id)
        .bind(&error)
        .execute(pool)
        .await?;
        tracing::warn!(job = %job_id, error = %error, "print job failed permanently");
    } else {
        let available_at_ms = now_ms() + RETRY_BASE_MS * attempt;
        sqlx::query(
            "UPDATE print_job
             SET status = 'pending', last_error = ?2, claimed_at_ms = NULL,
                 available_at_ms = ?3
             WHERE id = ?1",
        )
        .bind(job_id)
        .bind(&error)
        .bind(available_at_ms)
        .execute(pool)
        .await?;
        tracing::warn!(
            job = %job_id,
            error = %error,
            attempt,
            "print job failed; will retry"
        );
    }

    Ok(())
}

async fn recover_interrupted(pool: &SqlitePool) -> Result<(), AppError> {
    let updated = sqlx::query(
        "UPDATE print_job
         SET status = 'pending', claimed_at_ms = NULL, available_at_ms = 0,
             last_error = 'server restarted while printing'
         WHERE status = 'printing'",
    )
    .execute(pool)
    .await?;

    if updated.rows_affected() > 0 {
        tracing::warn!(
            jobs = updated.rows_affected(),
            "requeued interrupted print jobs"
        );
    }

    Ok(())
}

/// Runs the printing worker until the shutdown watch is set.
pub async fn run_worker(pool: SqlitePool, mut shutdown: watch::Receiver<bool>) {
    if let Err(error) = recover_interrupted(&pool).await {
        tracing::warn!(%error, "could not recover interrupted print jobs");
    }

    loop {
        if *shutdown.borrow() {
            break;
        }

        match process_next(&pool).await {
            Ok(true) => continue,
            Ok(false) => {}
            Err(error) => tracing::warn!(%error, "printing worker error"),
        }

        tokio::select! {
            () = tokio::time::sleep(POLL_INTERVAL) => {}
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }

    tracing::info!("printing worker stopped");
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use nexo_core::{BranchId, DeviceId, Operation, OperationKind, Uuid};
    use serde_json::json;
    use sqlx::Row;
    use tokio::io::AsyncReadExt;

    use super::*;
    use crate::test_support;

    fn sample_ticket() -> Ticket {
        Ticket {
            kind: "kitchen".to_owned(),
            business_name: "Café Central".to_owned(),
            printer_name: "Cocina abajo".to_owned(),
            order_label: "Mesa 4".to_owned(),
            order_code: "M-001".to_owned(),
            order_type: "dine_in".to_owned(),
            round_number: Some(2),
            sent_at_ms: 1_700_000_000_000,
            sent_by: "Yoel".to_owned(),
            reprint: false,
            fallback: false,
            lines: vec![
                TicketLine {
                    quantity: 2,
                    name: "Perro clásico".to_owned(),
                    notes: "Sin cebolla".to_owned(),
                },
                TicketLine {
                    quantity: 1,
                    name: "Papas fritas".to_owned(),
                    notes: String::new(),
                },
            ],
        }
    }

    #[test]
    fn ticket_renders_accents_in_cp850_without_prices() {
        let bytes = render_ticket(&sample_ticket(), 80);

        assert!(bytes.starts_with(&[0x1B, 0x40]));
        assert!(bytes.windows(3).any(|window| window == [0x1B, 0x74, 0x02]));
        assert!(bytes.contains(&0xA0), "á must be encoded as CP850 0xA0");
        assert!(bytes.contains(&0x82), "é must be encoded as CP850 0x82");
        assert!(bytes.windows(5).any(|window| window == b"COMAN"));
        assert!(bytes.windows(5).any(|window| window == b"2x Pe"));
        assert!(bytes.windows(5).any(|window| window == b"Papas"));
        assert!(!bytes.windows(4).any(|window| window == b"2.50"));
        assert!(!bytes.windows(3).any(|window| window == b"USD"));
    }

    #[test]
    fn long_lines_wrap_to_the_paper_width() {
        let mut ticket = sample_ticket();
        ticket.business_name = "Cafe Central".to_owned();
        ticket.printer_name = "Cocina".to_owned();
        ticket.order_label = "Mesa 4".to_owned();
        ticket.sent_by = "Yoel".to_owned();
        ticket.lines = vec![TicketLine {
            quantity: 1,
            name: "Perro clasico con papas fritas y una salsa de ajo muy cremosa".to_owned(),
            notes: String::new(),
        }];
        let bytes = render_ticket(&ticket, 58);
        let text = String::from_utf8(bytes).unwrap();

        assert!(text.contains("Perro clasico"));
        assert!(text.contains("cremosa"));
        for line in text.lines() {
            if line.chars().any(char::is_control) {
                continue;
            }
            assert!(line.chars().count() <= 32, "line too wide: {line:?}");
        }
    }

    #[test]
    fn reprint_and_fallback_are_marked_on_the_ticket() {
        let mut ticket = sample_ticket();
        ticket.reprint = true;
        ticket.fallback = true;
        let bytes = render_ticket(&ticket, 80);
        let text = String::from_utf8_lossy(&bytes);

        assert!(text.contains("REIMPRESION"));
        assert!(text.contains("SIN CATEGORIA"));
    }

    #[tokio::test]
    async fn a_sent_round_enqueues_one_job_per_printer_without_prices() {
        let state = test_support::state().await;
        let snapshot = crate::snapshot::sample_snapshot();
        let category_id = snapshot.categories[0].id.clone();
        let item_id = snapshot.items[0].id.clone();
        crate::snapshot::link(
            &state.pool,
            crate::snapshot::LinkRequest {
                server_user: "Admin".to_owned(),
                server_pin: "1234".to_owned(),
                snapshot,
            },
        )
        .await
        .unwrap();

        let business_id: String =
            sqlx::query_scalar("SELECT id FROM business ORDER BY created_at_ms LIMIT 1")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        let branch_id: String = sqlx::query_scalar("SELECT id FROM branch LIMIT 1")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let device_id: String = sqlx::query_scalar("SELECT id FROM device LIMIT 1")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let user_id: String = sqlx::query_scalar("SELECT id FROM app_user LIMIT 1")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let table_id: String = sqlx::query_scalar("SELECT id FROM dining_table LIMIT 1")
            .fetch_one(&state.pool)
            .await
            .unwrap();

        let now = now_ms();
        let printer_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO printer (
                id, business_id, name, host, port, paper_width_mm, is_active,
                created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, 'Cocina', '127.0.0.1', 9100, 80, 1, ?3, ?3)",
        )
        .bind(&printer_id)
        .bind(&business_id)
        .bind(now)
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query("UPDATE menu_category SET printer_id = ?2 WHERE id = ?1")
            .bind(&category_id)
            .bind(&printer_id)
            .execute(&state.pool)
            .await
            .unwrap();

        let order_id = Uuid::now_v7().to_string();
        let line_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO restaurant_order (
                id, business_id, branch_id, device_id, order_type, table_id,
                display_code, status, opened_by, opened_at_ms
             ) VALUES (?1, ?2, ?3, ?4, 'dine_in', ?5, 'M-001', 'open', ?6, ?7)",
        )
        .bind(&order_id)
        .bind(&business_id)
        .bind(&branch_id)
        .bind(&device_id)
        .bind(&table_id)
        .bind(&user_id)
        .bind(now)
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO order_line (
                id, order_id, menu_item_id, description, quantity,
                unit_price_minor, line_total_minor, currency, notes, status,
                round_number, created_at_ms, created_by
             ) VALUES (?1, ?2, ?3, 'Arepa', 2, 250, 500, 'USD', 'Sin queso',
                       'pending', 1, ?4, ?5)",
        )
        .bind(&line_id)
        .bind(&order_id)
        .bind(&item_id)
        .bind(now)
        .bind(&user_id)
        .execute(&state.pool)
        .await
        .unwrap();

        let operation = Operation::new(
            device_id.parse::<DeviceId>().unwrap(),
            Some(branch_id.parse::<BranchId>().unwrap()),
            OperationKind::OrderSent,
            order_id.parse::<Uuid>().unwrap(),
            json!({
                "round_number": 1,
                "line_ids": [line_id.clone()],
                "sent_by": user_id.clone(),
                "sent_at_ms": now,
            }),
            now,
        );
        let mut transaction = state.pool.begin().await.unwrap();
        crate::projection::apply(&mut transaction, &operation)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let jobs = sqlx::query("SELECT payload, kind FROM print_job")
            .fetch_all(&state.pool)
            .await
            .unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].try_get::<String, _>("kind").unwrap(), "kitchen");
        let payload: String = jobs[0].try_get("payload").unwrap();
        assert!(payload.contains("Arepa"));
        assert!(payload.contains("Sin queso"));
        let ticket: serde_json::Value = serde_json::from_str(&payload).unwrap();
        let line = &ticket["lines"][0];
        assert_eq!(line["name"], "Arepa");
        assert!(line.get("price").is_none());
        assert!(line.get("unit_price_minor").is_none());
    }

    #[tokio::test]
    async fn worker_sends_a_job_to_a_mock_printer() {
        let state = test_support::state().await;
        let business_id = seed_business(&state).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let printer_id = insert_printer(&state, &business_id, "Cocina", port).await;

        let mut transaction = state.pool.begin().await.unwrap();
        enqueue_test(&mut transaction, &business_id, &printer_id, "Café Central")
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            socket.read_to_end(&mut bytes).await.unwrap();
            bytes
        });

        assert!(process_next(&state.pool).await.unwrap());
        let bytes = tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();

        assert!(bytes.windows(5).any(|window| window == b"PRUEB"));
        let status: String = sqlx::query_scalar("SELECT status FROM print_job LIMIT 1")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "printed");
    }

    #[tokio::test]
    async fn worker_retries_a_failed_connection() {
        let state = test_support::state().await;
        let business_id = seed_business(&state).await;
        let port = unused_port().await;
        let printer_id = insert_printer(&state, &business_id, "Cocina", port).await;

        let mut transaction = state.pool.begin().await.unwrap();
        enqueue_test(&mut transaction, &business_id, &printer_id, "Café Central")
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        assert!(process_next(&state.pool).await.unwrap());
        let row = sqlx::query("SELECT status, attempts, last_error FROM print_job LIMIT 1")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(row.try_get::<String, _>("status").unwrap(), "pending");
        assert_eq!(row.try_get::<i64, _>("attempts").unwrap(), 1);
        assert!(!row.try_get::<String, _>("last_error").unwrap().is_empty());
    }

    async fn seed_business(state: &crate::state::AppState) -> String {
        sqlx::query_scalar("SELECT id FROM business ORDER BY created_at_ms LIMIT 1")
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    async fn insert_printer(
        state: &crate::state::AppState,
        business_id: &str,
        name: &str,
        port: u16,
    ) -> String {
        let printer_id = Uuid::now_v7().to_string();
        let now = now_ms();
        sqlx::query(
            "INSERT INTO printer (
                id, business_id, name, host, port, paper_width_mm, is_active,
                created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, '127.0.0.1', ?4, 80, 1, ?5, ?5)",
        )
        .bind(&printer_id)
        .bind(business_id)
        .bind(name)
        .bind(i64::from(port))
        .bind(now)
        .execute(&state.pool)
        .await
        .unwrap();
        printer_id
    }

    async fn unused_port() -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap().port()
    }
}

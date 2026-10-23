//! Full-screen menus and text input on stderr, shared by the profile selector and plugins.
//! Stdout stays free for machine-readable output.
use crossterm::{
  cursor::{Hide, MoveTo, Show},
  event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
  execute, queue,
  style::{Color, Print, ResetColor, SetForegroundColor},
  terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::{
  io::{self, Write},
  sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::Receiver,
  },
  time::Duration,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// The banner's colors from its left edge to its right: orange to yellow.
const BANNER_COLORS: [(u8, u8, u8); 2] = [(255, 120, 0), (255, 220, 60)];

/// Set while a `Hold` keeps the alternate screen between menus.
static HELD: AtomicBool = AtomicBool::new(false);

/// Raw mode, and the alternate screen unless a `Hold` keeps it, for as long as the value lives.
struct TerminalSession;

impl TerminalSession {
  /// Enable raw mode and, unless a `Hold` keeps it, the alternate screen.
  fn enter() -> io::Result<Self> {
    terminal::enable_raw_mode()?;
    let session = Self;
    if !HELD.load(Ordering::SeqCst) {
      execute!(io::stderr(), EnterAlternateScreen, Hide)?;
    }
    Ok(session)
  }
}

impl Drop for TerminalSession {
  fn drop(&mut self) {
    if !HELD.load(Ordering::SeqCst) {
      let _ = execute!(io::stderr(), LeaveAlternateScreen, Show, ResetColor);
    }
    let _ = terminal::disable_raw_mode();
  }
}

/// Keeps the alternate screen between menus for as long as it lives, so the shell does
/// not show while awshop waits for AWS. Messages for the user go out after it is dropped.
pub struct Hold(bool);

/// Hold the alternate screen. Between menus the terminal is not in raw mode, so Ctrl-C
/// sends SIGINT; the handler then restores the terminal before exiting.
pub fn hold() -> io::Result<Hold> {
  if HELD.swap(true, Ordering::SeqCst) {
    return Ok(Hold(false));
  }

  let held = Hold(true);

  // The handler goes first, so a Ctrl-C can never leave the alternate screen behind.
  // SAFETY: the handler only calls async-signal-safe functions.
  if unsafe {
    libc::signal(
      libc::SIGINT,
      interrupted as extern "C" fn(libc::c_int) as libc::sighandler_t,
    )
  } == libc::SIG_ERR
  {
    return Err(io::Error::last_os_error());
  }

  execute!(io::stderr(), EnterAlternateScreen, Hide)?;
  Ok(held)
}

impl Drop for Hold {
  fn drop(&mut self) {
    if self.0 {
      release();
    }
  }
}

/// Leave a held screen early, such as before another program takes the terminal.
pub fn release() {
  if HELD.swap(false, Ordering::SeqCst) {
    // SAFETY: restores the default action after leaving the held screen.
    if unsafe { libc::signal(libc::SIGINT, libc::SIG_DFL) } == libc::SIG_ERR {
      eprintln!(
        "awshop: Could not restore Ctrl-C handler: {}",
        io::Error::last_os_error()
      );
    }
    let _ = execute!(io::stderr(), LeaveAlternateScreen, Show, ResetColor);
  }
}

/// While it lives, awshop ignores Ctrl-C and Ctrl-\, which then reach only a program it
/// waits for, as `system(3)` does. Some programs pass them on: an SSM shell sends them
/// to the remote host. Create it after the program started, which keeps the defaults.
pub struct Interrupts {
  sigint: libc::sighandler_t,
  sigquit: libc::sighandler_t,
}

impl Interrupts {
  /// Ignore Ctrl-C and Ctrl-\ until the value is dropped.
  pub fn ignore() -> io::Result<Interrupts> {
    // SAFETY: changes signal dispositions only while awshop waits for the child.
    let sigint = unsafe { libc::signal(libc::SIGINT, libc::SIG_IGN) };
    if sigint == libc::SIG_ERR {
      return Err(io::Error::last_os_error());
    }

    let sigquit = unsafe { libc::signal(libc::SIGQUIT, libc::SIG_IGN) };
    if sigquit == libc::SIG_ERR {
      let error = io::Error::last_os_error();
      if unsafe { libc::signal(libc::SIGINT, sigint) } == libc::SIG_ERR {
        eprintln!(
          "awshop: Could not restore Ctrl-C handler: {}",
          io::Error::last_os_error()
        );
      }
      return Err(error);
    }

    Ok(Interrupts { sigint, sigquit })
  }
}

impl Drop for Interrupts {
  fn drop(&mut self) {
    // SAFETY: restores the actions from before `ignore`.
    for (signal, previous, name) in [
      (libc::SIGINT, self.sigint, "Ctrl-C"),
      (libc::SIGQUIT, self.sigquit, "Ctrl-\\"),
    ] {
      if unsafe { libc::signal(signal, previous) } == libc::SIG_ERR {
        eprintln!(
          "awshop: Could not restore {name} handler: {}",
          io::Error::last_os_error()
        );
      }
    }
  }
}

/// SIGINT while a `Hold` waits outside a menu: leave the alternate screen, show the cursor
/// and exit as the default action would.
extern "C" fn interrupted(_: libc::c_int) {
  // Leave the alternate screen, show the cursor, reset colors.
  const RESTORE: &[u8] = b"\x1b[?1049l\x1b[?25h\x1b[0m";
  // SAFETY: write and _exit are async-signal-safe. A failed write cannot be recovered here.
  unsafe {
    let mut written = 0;
    while written < RESTORE.len() {
      let count = libc::write(2, RESTORE[written..].as_ptr().cast(), RESTORE.len() - written);
      if count <= 0 {
        break;
      }
      written += count as usize;
    }
    libc::_exit(130);
  }
}

/// Show `title` and `context` with a waiting note on a held screen while slow work runs.
pub fn busy(title: &str, context: &[String]) -> io::Result<()> {
  let (width, height) = terminal::size().unwrap_or((80, 24));
  let mut output = Vec::new();
  queue!(output, ResetColor, MoveTo(0, 0), Clear(ClearType::All))?;
  line(&mut output, 0, title, width, Color::Cyan)?;
  let rows = context.len().min(usize::from(height.saturating_sub(4)));
  for (index, text) in context.iter().take(rows).enumerate() {
    line(&mut output, 2 + index as u16, text, width, Color::Reset)?;
  }
  line(&mut output, 3 + rows as u16, "Waiting for AWS…", width, Color::DarkGrey)?;
  line(
    &mut output,
    height.saturating_sub(1),
    "^C cancel",
    width,
    Color::DarkGrey,
  )?;

  io::stderr().write_all(&output)?;
  io::stderr().flush()
}

/// A menu row. The description is shown below the list while the row is highlighted.
#[derive(Clone)]
pub struct Item {
  pub label: String,
  /// Detail columns after the label, lined up across rows.
  pub columns: Vec<String>,
  pub description: String,
  /// Text the search matches without showing it.
  pub keywords: String,
}

impl Item {
  /// A row with a label and a detail column.
  pub fn new(label: impl Into<String>, detail: impl Into<String>) -> Self {
    Self::columns(label, vec![detail.into()])
  }

  /// A row with a label and any number of detail columns.
  pub fn columns(label: impl Into<String>, columns: Vec<String>) -> Self {
    Self {
      label: label.into(),
      columns,
      description: String::new(),
      keywords: String::new(),
    }
  }

  /// The text shown below the list while the row is highlighted.
  pub fn describe(mut self, description: impl Into<String>) -> Self {
    self.description = description.into();
    self
  }

  /// Text the search matches in addition to the label and detail.
  pub fn keywords(mut self, keywords: impl Into<String>) -> Self {
    self.keywords = keywords.into();
    self
  }

  /// Case-insensitive search over the label, columns and keywords.
  pub fn matches(&self, query: &str) -> bool {
    format!("{} {} {}", self.label, self.columns.join(" "), self.keywords)
      .to_lowercase()
      .contains(&query.to_lowercase())
  }
}

/// How a menu was left. Selected and Open hold the index into `Menu::items`; Open, for
/// a menu with `opens`, also holds the search text, so the caller can show it again.
/// Refresh, sent through `Menu::refresh`, holds the highlighted index and the search.
pub enum Choice {
  Selected(usize),
  Open(usize, String),
  Refresh(Option<usize>, String),
  Back,
  Shortcut(char),
}

/// A menu to show with `choose`; `Menu::new` gives the defaults.
#[derive(Clone, Copy)]
pub struct Menu<'a> {
  pub title: &'a str,
  /// Lines between the title and the list, such as the current profile and region.
  pub context: &'a [String],
  pub items: &'a [Item],
  /// Column headings above the list, the label's first; none shows no heading row.
  pub headings: &'a [&'a str],
  /// Some enables type-to-filter, starting with this text.
  pub search: Option<&'a str>,
  /// Initial position among the items that match the initial search.
  pub selected: usize,
  /// Ctrl-key shortcuts returned as `Choice::Shortcut`, with help labels.
  pub shortcuts: &'a [(char, &'a str)],
  /// Enter returns `Choice::Open`: each item opens a menu of its own, and going back
  /// from it should find this one as it was.
  pub opens: bool,
  /// The first menu of a flow. The left arrow goes back everywhere else, but here going
  /// back means exiting, which only Escape does.
  pub root: bool,
  /// Art drawn centered above the search when the terminal has room for it.
  pub banner: &'a [&'a str],
  /// New columns for items, by index, shown as they arrive.
  pub updates: Option<&'a Receiver<(usize, Vec<String>)>>,
  /// A message here returns `Choice::Refresh`, so the caller can rebuild the items.
  pub refresh: Option<&'a Receiver<()>>,
  /// Shown when no item matches.
  pub empty: &'a str,
}

impl<'a> Menu<'a> {
  /// A plain menu without context, search or shortcuts.
  pub fn new(title: &'a str, items: &'a [Item]) -> Self {
    Self {
      title,
      context: &[],
      items,
      headings: &[],
      search: None,
      selected: 0,
      shortcuts: &[],
      opens: false,
      root: false,
      banner: &[],
      updates: None,
      refresh: None,
      empty: "",
    }
  }
}

/// Show a menu until the user selects, goes back or presses a shortcut.
/// Ctrl-C returns an `Interrupted` error so callers can abort the whole flow.
pub fn choose(menu: Menu<'_>) -> io::Result<Choice> {
  let _terminal = TerminalSession::enter()?;

  // Updates change the rows, so the menu works on its own copy.
  let mut items = menu.items.to_vec();
  let mut filter = menu.search.unwrap_or_default().to_owned();
  let mut selected = menu.selected;
  let mut dirty = true;
  let mut page = 1;
  let mut search_visible = !filter.is_empty();
  let mut searching = false;

  loop {
    let matches: Vec<_> = items
      .iter()
      .enumerate()
      .filter(|(_, item)| menu.search.is_none() || item.matches(&filter))
      .map(|(index, _)| index)
      .collect();
    selected = selected.min(matches.len().saturating_sub(1));

    let mut help = "↑/↓ · enter select".to_owned();
    help.push_str(if menu.root { " · esc back" } else { " · ←/esc back" });
    for (key, label) in menu.shortcuts {
      help.push_str(&format!(" · ^{} {label}", key.to_ascii_uppercase()));
    }
    if menu.search.is_some() {
      help.push_str(if searching {
        " · esc to exit search"
      } else {
        " · / search"
      });
    }
    help.push_str(" · ^C exit");
    let search = search_visible.then(|| format!("Search: {filter}"));
    if dirty {
      page = draw(
        &Menu { items: &items, ..menu },
        &matches,
        selected,
        search.as_deref(),
        &help,
      )?;
      dirty = false;
    }

    // While updates may come, look for them between keys.
    if (menu.updates.is_some() || menu.refresh.is_some()) && !event::poll(Duration::from_millis(100))? {
      if menu.refresh.is_some_and(|refresh| refresh.try_recv().is_ok()) {
        return Ok(Choice::Refresh(matches.get(selected).copied(), filter));
      }
      for (index, columns) in menu.updates.into_iter().flat_map(Receiver::try_iter) {
        if let Some(item) = items.get_mut(index) {
          item.columns = columns;
          dirty = true;
        }
      }
      continue;
    }

    dirty = true;
    if let Event::Key(key) = event::read()? {
      if key.kind == KeyEventKind::Release {
        continue;
      }

      if key.modifiers.contains(KeyModifiers::CONTROL) {
        if key.code == KeyCode::Char('c') {
          return Err(cancelled());
        }
        if let KeyCode::Char(character) = key.code {
          if menu.shortcuts.iter().any(|(shortcut, _)| *shortcut == character) {
            return Ok(Choice::Shortcut(character));
          }
        }
        continue;
      }

      match key.code {
        KeyCode::Esc if !searching => return Ok(Choice::Back),
        KeyCode::Left if !menu.root => return Ok(Choice::Back),
        KeyCode::Enter if menu.opens => {
          if let Some(index) = matches.get(selected) {
            return Ok(Choice::Open(*index, filter));
          }
        }
        KeyCode::Enter => {
          if let Some(index) = matches.get(selected) {
            return Ok(Choice::Selected(*index));
          }
        }
        KeyCode::Esc if searching => searching = false,
        KeyCode::Char('/') if menu.search.is_some() => {
          search_visible = true;
          searching = true;
        }
        KeyCode::Char('j') if !searching => selected = (selected + 1).min(matches.len().saturating_sub(1)),
        KeyCode::Char('k') if !searching => selected = selected.saturating_sub(1),
        KeyCode::Up => selected = selected.saturating_sub(1),
        KeyCode::Down => selected = (selected + 1).min(matches.len().saturating_sub(1)),
        KeyCode::PageUp => selected = selected.saturating_sub(page),
        KeyCode::PageDown => selected = (selected + page).min(matches.len().saturating_sub(1)),
        KeyCode::Home => selected = 0,
        KeyCode::End => selected = matches.len().saturating_sub(1),
        KeyCode::Backspace if searching => {
          filter.pop();
          selected = 0;
        }
        KeyCode::Char(character) if searching && !character.is_control() => {
          filter.push(character);
          selected = 0;
        }
        _ => {}
      }
    }
  }
}

/// Cut a string to a number of terminal columns, dropping control characters.
fn truncate(value: &str, max_columns: usize) -> String {
  let mut result = String::new();
  let mut columns = 0;

  for character in value.chars().filter(|character| !character.is_control()) {
    let width = character.width().unwrap_or(0);
    if columns + width > max_columns {
      break;
    }
    result.push(character);
    columns += width;
  }

  result
}

/// Write one row, cut short of the last column so the terminal never wraps.
fn line(out: &mut impl Write, row: u16, text: &str, width: u16, color: Color) -> io::Result<()> {
  queue!(
    out,
    MoveTo(0, row),
    SetForegroundColor(color),
    Print(truncate(text, usize::from(width.saturating_sub(1)))),
    ResetColor
  )
}

/// Render a frame off-screen and write it at once to avoid flicker.
/// Returns how many list rows fit, which is one page.
fn draw(menu: &Menu<'_>, matches: &[usize], selected: usize, search: Option<&str>, help: &str) -> io::Result<usize> {
  let mut output = Vec::new();
  let page = draw_frame(
    &mut output,
    terminal::size().unwrap_or((80, 24)),
    menu,
    matches,
    selected,
    search,
    help,
  )?;

  let mut stderr = io::stderr().lock();
  stderr.write_all(&output)?;
  stderr.flush()?;
  Ok(page)
}

/// Layout from the top: title, context, search, list; from the bottom: help, then
/// the description rows that fit. Rows are dropped in that order as the terminal shrinks.
fn draw_frame(
  out: &mut impl Write,
  (width, height): (u16, u16),
  menu: &Menu<'_>,
  matches: &[usize],
  selected: usize,
  search: Option<&str>,
  help: &str,
) -> io::Result<usize> {
  queue!(out, ResetColor, MoveTo(0, 0), Clear(ClearType::All))?;
  if height < 6 {
    line(out, 0, "Resize terminal to continue", width, Color::Yellow)?;
    return Ok(1);
  }

  line(out, 0, menu.title, width, Color::Cyan)?;
  let status_rows = if height >= 10 {
    2
  } else if height >= 8 {
    1
  } else {
    0
  };

  // Keep at least one choice visible, including when the summary is long.
  let context_rows = menu
    .context
    .len()
    .min(usize::from(height.saturating_sub(7 + status_rows)));
  for (index, text) in menu.context.iter().take(context_rows).enumerate() {
    line(out, 2 + index as u16, text, width, Color::Reset)?;
  }

  // The banner goes first when rows run short: it needs room for six list rows besides.
  let heading_rows = usize::from(!menu.headings.is_empty());
  let banner_width = menu.banner.iter().map(|line| line.width()).max().unwrap_or(0);
  let banner_rows = if usize::from(height)
    >= 2 + context_rows + menu.banner.len() + 1 + heading_rows + 6 + 1 + usize::from(status_rows)
    && usize::from(width) > banner_width
  {
    menu.banner.len()
  } else {
    0
  };

  let indent = usize::from(width).saturating_sub(banner_width) / 2;
  let [from, to] = BANNER_COLORS;
  for (index, art) in menu.banner.iter().take(banner_rows).enumerate() {
    queue!(out, MoveTo(indent as u16, (2 + context_rows + index) as u16))?;

    // It fits the width, so it needs no truncation; each column gets its own color.
    for (column, character) in art.chars().enumerate() {
      let share = column as f32 / banner_width.saturating_sub(1).max(1) as f32;
      let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * share).round() as u8;
      let color = Color::Rgb {
        r: mix(from.0, to.0),
        g: mix(from.1, to.1),
        b: mix(from.2, to.2),
      };
      queue!(out, SetForegroundColor(color), Print(character))?;
    }

    queue!(out, ResetColor)?;
  }

  let search_row = (2 + context_rows + banner_rows) as u16;
  if let Some(search) = search {
    line(out, search_row, search, width, Color::Reset)?;
  }

  let start = search_row + 1 + heading_rows as u16;
  let visible = usize::from(height.saturating_sub(start + 2 + status_rows)).max(1);
  let first = selected.saturating_sub(visible - 1);

  // Columns are as wide as their widest cell or heading, over all items so they stay put
  // while searching; the labels take at most half the width. A column that is empty in
  // every row is left out with its heading (None). Columns that don't fit are cut off at
  // the right edge.
  let heading = |index: usize| menu.headings.get(index).copied().unwrap_or("");
  let column_count = menu.items.iter().map(|item| item.columns.len()).max().unwrap_or(0);
  let widths: Vec<_> = (0..=column_count)
    .map(|index| {
      let widest = menu
        .items
        .iter()
        .map(|item| match index {
          0 => item.label.width(),
          _ => item.columns.get(index - 1).map_or(0, |cell| cell.width()),
        })
        .max()
        .unwrap_or(0);
      match index {
        0 => Some(widest.max(heading(0).width()).min(usize::from(width) / 2)),
        _ if widest == 0 => None,
        _ => Some(widest.max(heading(index).width())),
      }
    })
    .collect();
  let row = |marker: &str, cells: Vec<&str>| {
    let mut text = format!("{marker} ");
    for (cell, column) in cells.iter().zip(&widths) {
      let Some(column) = column else {
        continue;
      };
      text.push_str(cell);
      text.push_str(&" ".repeat(column.saturating_sub(cell.width()) + 2));
    }
    text.trim_end().to_owned()
  };

  if heading_rows > 0 {
    let cells = (0..widths.len()).map(heading).collect();
    line(out, search_row + 1, &row(" ", cells), width, Color::DarkGrey)?;
  }

  for (offset, position) in (first..(first + visible).min(matches.len())).enumerate() {
    let item = &menu.items[matches[position]];
    let marker = if position == selected { "›" } else { " " };
    let cells = [item.label.as_str()]
      .into_iter()
      .chain(item.columns.iter().map(String::as_str))
      .collect();
    let text = row(marker, cells);
    line(
      out,
      start + offset as u16,
      &text,
      width,
      if position == selected {
        Color::Yellow
      } else {
        Color::Reset
      },
    )?;
  }

  if matches.is_empty() {
    line(out, start, menu.empty, width, Color::Yellow)?;
  }

  if status_rows > 0 {
    let description = matches
      .get(selected)
      .map(|index| menu.items[*index].description.as_str())
      .unwrap_or("No matching items. Change the search or go back.");
    let lines = status_lines(
      description,
      usize::from(width.saturating_sub(1)),
      usize::from(status_rows),
    );
    for (index, text) in lines.iter().enumerate() {
      line(out, height - 1 - status_rows + index as u16, text, width, Color::Reset)?;
    }
  }

  line(out, height - 1, help, width, Color::DarkGrey)?;
  Ok(visible)
}

/// Word-wrap a description into at most `rows` lines.
fn status_lines(description: &str, width: usize, rows: usize) -> Vec<String> {
  let mut lines = Vec::new();
  let mut current = String::new();

  for word in description.split_whitespace() {
    let space = usize::from(!current.is_empty());
    if current.width() + space + word.width() > width && !current.is_empty() {
      lines.push(current);
      current = String::new();
      if lines.len() == rows {
        return lines;
      }
    }

    if !current.is_empty() {
      current.push(' ');
    }
    current.push_str(&truncate(word, width));
  }

  if !current.is_empty() {
    lines.push(current);
  }
  lines
}

/// Edit one line of text. Returns None on Escape; Ctrl-C cancels the whole flow.
pub fn input(title: &str, label: &str, initial: &str) -> io::Result<Option<String>> {
  let _terminal = TerminalSession::enter()?;
  let mut value = initial.to_owned();
  loop {
    let (width, height) = terminal::size().unwrap_or((80, 24));
    let mut output = Vec::new();
    queue!(output, ResetColor, MoveTo(0, 0), Clear(ClearType::All))?;
    if height < 6 {
      line(&mut output, 0, "Resize terminal to continue", width, Color::Yellow)?;
    } else {
      line(&mut output, 0, title, width, Color::Cyan)?;
      line(&mut output, 2, label, width, Color::Reset)?;

      // Keep the end of long input (for example a role ARN) visible.
      let mut tail = value.as_str();
      let columns = usize::from(width.saturating_sub(4));
      while tail.width() > columns {
        tail = &tail[tail.chars().next().map(char::len_utf8).unwrap_or(0)..];
      }

      line(&mut output, 3, &format!("> {tail}▏"), width, Color::Yellow)?;
      line(
        &mut output,
        height - 1,
        "enter accept · esc back · ctrl-u clear · ctrl-c exit",
        width,
        Color::DarkGrey,
      )?;
    }

    io::stderr().write_all(&output)?;
    io::stderr().flush()?;

    if let Event::Key(key) = event::read()? {
      if key.kind == KeyEventKind::Release {
        continue;
      }

      match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Err(cancelled()),
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => value.clear(),
        KeyCode::Esc => return Ok(None),
        KeyCode::Enter => return Ok(Some(value.trim().to_owned())),
        KeyCode::Backspace => {
          value.pop();
        }
        KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) && !character.is_control() => {
          value.push(character)
        }
        _ => {}
      }
    }
  }
}

/// Show a message, one row per line so long errors can be scrolled.
pub fn notice(title: &str, message: &str) -> io::Result<()> {
  let lines: Vec<_> = message.lines().map(|line| Item::new(line, "")).collect();
  choose(Menu {
    empty: "Press Esc to return",
    ..Menu::new(title, &lines)
  })?;
  Ok(())
}

/// The error for Ctrl-C in a menu, which ends the whole flow quietly.
fn cancelled() -> io::Error {
  io::Error::new(io::ErrorKind::Interrupted, "Cancelled")
}

#[cfg(test)]
mod tests {
  use super::*;

  fn selector(items: &[Item]) -> Menu<'_> {
    Menu {
      search: Some(""),
      empty: "No profiles match this filter.",
      ..Menu::new("AWS profile selector", items)
    }
  }

  /// Draw a frame onto an existing virtual terminal.
  fn render(terminal: &mut vt100::Parser, menu: &Menu<'_>, matches: &[usize], selected: usize, search: Option<&str>) {
    let (height, width) = terminal.screen().size();
    let mut bytes = Vec::new();
    draw_frame(
      &mut bytes,
      (width, height),
      menu,
      matches,
      selected,
      search,
      "enter select",
    )
    .unwrap();
    terminal.process(&bytes);
  }

  fn test_frame(size: (u16, u16), items: &[Item], matches: &[usize], selected: usize, search: &str) -> vt100::Parser {
    let mut terminal = vt100::Parser::new(size.1, size.0, 0);
    render(&mut terminal, &selector(items), matches, selected, Some(search));
    terminal
  }

  #[test]
  fn rows_stay_aligned_and_selected_item_stays_visible() {
    let items: Vec<_> = (0..30)
      .map(|index| Item::new(format!("demo-{index:02}"), "AWS profile"))
      .collect();
    let candidates: Vec<_> = (0..30).collect();
    let terminal = test_frame((80, 8), &items, &candidates, 29, "Search: ");
    let rows: Vec<_> = terminal.screen().rows(0, 80).collect();

    assert_eq!(rows[0], "AWS profile selector");
    assert_eq!(rows[2], "Search: ");
    assert!(rows[3].starts_with("  demo-28"));
    assert!(rows[4].starts_with("› demo-29"));
    assert_eq!(rows[6], "");
  }

  #[test]
  fn wide_characters_and_long_filters_do_not_wrap() {
    let items = [Item::new("界".repeat(30), "")];
    let terminal = test_frame((20, 6), &items, &[0], 0, &format!("Search: {}", "界".repeat(30)));
    let rows: Vec<_> = terminal.screen().rows(0, 20).collect();

    assert_eq!(rows[0], "AWS profile selecto");
    assert_eq!(rows[2], format!("Search: {}", "界".repeat(5)));
    assert_eq!(rows[3], format!("› {}", "界".repeat(8)));
    assert!((0..6).all(|row| !terminal.screen().row_wrapped(row)));
  }

  #[test]
  fn empty_results_clear_previous_rows() {
    let items = [Item::new("dev", ""), Item::new("prod", "")];
    let mut terminal = test_frame((80, 10), &items, &[0, 1], 0, "Search: ");
    render(&mut terminal, &selector(&items), &[], 0, Some("Search: missing"));

    let rows: Vec<_> = terminal.screen().rows(0, 80).collect();
    assert_eq!(rows[3], "No profiles match this filter.");
    assert!(rows[4..7].iter().all(String::is_empty));
    assert!(rows[7].starts_with("No matching items."));
  }

  #[test]
  fn banner_is_centered_above_the_search_and_dropped_when_rows_run_short() {
    let items = [Item::new("dev", "")];
    let menu = Menu {
      banner: &["/\\", "\\/"],
      ..selector(&items)
    };

    let mut terminal = vt100::Parser::new(16, 20, 0);
    render(&mut terminal, &menu, &[0], 0, Some("Search: "));
    let rows: Vec<_> = terminal.screen().rows(0, 20).collect();
    assert_eq!(rows[2], "         /\\");
    assert_eq!(rows[3], "         \\/");
    assert_eq!(rows[4], "Search: ");

    let color = |column| terminal.screen().cell(2, column).unwrap().fgcolor();
    assert_eq!(color(9), vt100::Color::Rgb(255, 120, 0));
    assert_eq!(color(10), vt100::Color::Rgb(255, 220, 60));

    let mut terminal = vt100::Parser::new(10, 20, 0);
    render(&mut terminal, &menu, &[0], 0, Some("Search: "));
    assert_eq!(terminal.screen().rows(0, 20).nth(2).unwrap(), "Search: ");

    // A terminal narrower than the banner gets none.
    let wide = Menu {
      banner: &["█".repeat(30).leak()],
      ..selector(&items)
    };
    let mut terminal = vt100::Parser::new(16, 20, 0);
    render(&mut terminal, &wide, &[0], 0, Some("Search: "));
    assert_eq!(terminal.screen().rows(0, 20).nth(2).unwrap(), "Search: ");
  }

  #[test]
  fn details_line_up_in_a_column() {
    let items = [
      Item::new("dev", "SSO session"),
      Item::new("production", "role via dev"),
      Item::new("界", "wide"),
    ];

    let terminal = test_frame((60, 12), &items, &[0, 1, 2], 0, "Search: ");
    let rows: Vec<_> = terminal.screen().rows(0, 60).collect();

    assert_eq!(rows[3], "› dev         SSO session");
    assert_eq!(rows[4], "  production  role via dev");
    assert_eq!(rows[5], "  界          wide");
  }

  #[test]
  fn columns_line_up_under_headings_and_are_cut_at_the_right_edge() {
    let row = |label: &str, cells: &[&str]| Item::columns(label, cells.iter().map(|cell| cell.to_string()).collect());
    let items = [
      row("dev", &["SSO corp", "111111111111", "eu-north-1"]),
      row("ci-deploy", &["via dev", "", "eu-west-1"]),
    ];
    let menu = Menu {
      headings: &["Profile", "Login", "Account", "Region"],
      ..selector(&items)
    };

    let mut terminal = vt100::Parser::new(12, 60, 0);
    render(&mut terminal, &menu, &[0, 1], 0, Some("Search: "));
    let rows: Vec<_> = terminal.screen().rows(0, 60).collect();
    assert_eq!(rows[3], "  Profile    Login     Account       Region");
    assert_eq!(rows[4], "› dev        SSO corp  111111111111  eu-north-1");
    assert_eq!(rows[5], "  ci-deploy  via dev                 eu-west-1");

    // A column that is empty in every row is left out, with its heading.
    let sparse = [row("dev", &["", "eu-north-1"]), row("prod", &["", "us-east-1"])];
    let menu_sparse = Menu {
      headings: &["Profile", "Login", "Region"],
      ..selector(&sparse)
    };
    let mut terminal = vt100::Parser::new(12, 60, 0);
    render(&mut terminal, &menu_sparse, &[0, 1], 0, Some("Search: "));
    let rows: Vec<_> = terminal.screen().rows(0, 60).collect();
    assert_eq!(rows[3], "  Profile  Region");
    assert_eq!(rows[4], "› dev      eu-north-1");

    let mut terminal = vt100::Parser::new(12, 30, 0);
    render(&mut terminal, &menu, &[0, 1], 0, Some("Search: "));
    assert_eq!(
      terminal.screen().rows(0, 30).nth(4).unwrap(),
      "› dev        SSO corp  111111"
    );
    assert!((0..12).all(|row| !terminal.screen().row_wrapped(row)));
  }

  #[test]
  fn tiny_terminal_displays_resize_message() {
    let terminal = test_frame((20, 2), &[], &[], 0, "");
    let rows: Vec<_> = terminal.screen().rows(0, 20).collect();
    assert_eq!(rows[0], "Resize terminal to ");
    assert!(rows[1].is_empty());
  }

  #[test]
  fn status_follows_the_highlighted_item_and_clears_previous_description() {
    let items = [
      Item::new("Authentication", "").describe("Choose the role kubectl uses to authenticate."),
      Item::new("Lookup role", "").describe("Retrieve cluster information from AWS."),
    ];
    let menu = Menu::new("EKS", &items);
    let mut terminal = vt100::Parser::new(12, 80, 0);

    for (selected, item) in items.iter().enumerate() {
      render(&mut terminal, &menu, &[0, 1], selected, None);
      let rows: Vec<_> = terminal.screen().rows(0, 80).collect();
      assert_eq!(rows[9], item.description);
      assert!(rows[10].is_empty());
      assert!((0..12).all(|row| !terminal.screen().row_wrapped(row)));
    }
  }

  #[test]
  fn summary_keeps_actions_visible_in_small_terminals() {
    let context = vec!["Setting: a long value".to_owned(); 10];
    let items = vec![Item::new("Configure and activate", "")];
    let menu = Menu {
      context: &context,
      ..Menu::new("awshop › EKS", &items)
    };

    for (width, height) in [(80, 24), (25, 8), (12, 6)] {
      let mut terminal = vt100::Parser::new(height, width, 0);
      render(&mut terminal, &menu, &[0], 0, None);
      assert!(terminal.screen().contents().contains("› Configure"));
      assert!((0..height).all(|row| !terminal.screen().row_wrapped(row)));
    }
  }
}

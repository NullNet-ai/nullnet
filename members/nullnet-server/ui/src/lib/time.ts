// Full localized date + time (24h), for anything not from today.
function dateTime(date: Date): string {
  return `${date.toLocaleDateString()} ${date.toLocaleTimeString([], { hour12: false })}`;
}

// Formats a unix-seconds timestamp for display: time-only (24h) when the
// timestamp falls on today's local calendar date, otherwise the full date
// and time — so a connection from days ago isn't shown identically to one
// just opened.
export function formatTimestamp(unix: number): string {
  const date = new Date(unix * 1000);
  const now = new Date();
  return date.toDateString() === now.toDateString()
    ? date.toLocaleTimeString([], { hour12: false })
    : dateTime(date);
}

// Full date + time regardless of how recent, for a title/tooltip attribute
// alongside the (possibly time-only) display string above.
export function formatTimestampFull(unix: number): string {
  return dateTime(new Date(unix * 1000));
}

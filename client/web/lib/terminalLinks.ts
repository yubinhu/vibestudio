import type { IBuffer, IBufferRange, ILink, ILinkProvider, Terminal } from "@xterm/xterm";

export interface FileLinkTarget {
  path: string;
  line?: number;
  column?: number;
}

interface FileLinkMatch extends FileLinkTarget {
  text: string;
  start: number;
  end: number;
}

/** Only web URLs may leave the app. OSC 8 output is untrusted terminal data. */
export function webLinkUrl(text: string): string | null {
  // eslint-disable-next-line no-control-regex -- Terminal output must not inject URL control characters.
  if (!/^https?:\/\//i.test(text) || /[\u0000-\u0020\u007f]/.test(text)) return null;
  try {
    const url = new URL(text);
    return url.hostname && !url.username && !url.password ? url.href : null;
  } catch { return null; }
}

export function parseFileLink(text: string): FileLinkTarget | null {
  // eslint-disable-next-line no-control-regex -- Reject raw terminal escape/control sequences before parsing.
  if (!text || /[\u0000-\u001f\u007f]/.test(text)) return null;
  let path = text;
  let line: number | undefined;
  let column: number | undefined;
  const location = /(?::(\d+)(?::(\d+))?|#L(\d+)(?:C(\d+))?|\((\d+)(?:,\s*(\d+))?\))$/.exec(path);
  if (location) {
    line = Number(location[1] ?? location[3] ?? location[5]);
    const rawColumn = location[2] ?? location[4] ?? location[6];
    column = rawColumn === undefined ? undefined : Number(rawColumn);
    if (!Number.isSafeInteger(line) || line < 1 || (column !== undefined && (!Number.isSafeInteger(column) || column < 1))) return null;
    path = path.slice(0, location.index);
  }
  if (/^file:\/\//i.test(path)) {
    try {
      const url = new URL(path);
      if (url.username || url.password || url.port || url.search || url.hash) return null;
      // The authority labels the emitting host. Files always resolve on the
      // active workspace server, never through a browser file:// navigation.
      path = decodeURIComponent(url.pathname);
    } catch { return null; }
  } else if (/^[a-z][a-z\d+.-]*:/i.test(path) && !/^[a-z]:[\\/]/i.test(path)) {
    return null;
  }
  // eslint-disable-next-line no-control-regex -- Decoding file URLs can introduce otherwise hidden control characters.
  if (!path || /[\u0000-\u001f\u007f]/.test(path)) return null;
  return { path, line, column };
}

/** Paths printed by shells, markdown, Rust/TS diagnostics and Python tracebacks.
 * Quoting is required for spaces, just as it is in shell output. Existence is
 * checked on activation, so hovering never performs filesystem/network work. */
export function detectFileLinks(text: string): FileLinkMatch[] {
  const result: FileLinkMatch[] = [];
  // URI authorities and queries may contain brackets, which also delimit
  // markdown paths below. Exclude the entire URI before tokenizing so an IPv6
  // address or query cannot turn its tail into a local filesystem link.
  const urls = Array.from(text.matchAll(/\b([a-z][a-z\d+.-]*):\/\/[^\s"'`<>]+/gi))
    .filter((match) => match[1].toLowerCase() !== "file")
    .map((match) => ({ start: match.index!, end: match.index! + match[0].length }));
  const tokens = /(["'`])((?:(?!\1)[^\r\n])+)\1(?::\d+(?::\d+)?|#L\d+(?:C\d+)?|\(\d+(?:,\s*\d+)?\))?|[^\s"'`<>[\]{}|]+?\(\d+,\s*\d+\)|[^\s"'`<>[\]{}|]+/g;
  for (const token of text.matchAll(tokens)) {
    if (urls.some((url) => token.index! < url.end && token.index! + token[0].length > url.start)) continue;
    let value = token[0];
    let start = token.index!;
    if (token[1]) {
      start++;
      value = token[2] + token[0].slice(token[2].length + 2);
    } else {
      const leading = /^\(+/.exec(value)?.[0].length ?? 0;
      start += leading;
      value = value.slice(leading).replace(/[.,;:!?]+$/, "");
      while (value.endsWith(")") && (value.match(/\)/g)?.length ?? 0) > (value.match(/\(/g)?.length ?? 0)) value = value.slice(0, -1);
    }
    const target = parseFileLink(value);
    if (!target || !(/[\\/]/.test(target.path) || /\.[\p{L}\p{N}_-]+$/u.test(target.path) || /^(?:Dockerfile|Makefile|LICENSE|README|AGENTS)$/i.test(target.path))) continue;
    // A URL must never yield a clickable filesystem fragment.
    if (/^https?:/i.test(value)) continue;
    if (!target.line && token[1]) {
      const pythonLine = /^,?\s+line (\d+)/.exec(text.slice(token.index! + token[0].length));
      if (pythonLine) {
        target.line = Number(pythonLine[1]);
        if (!Number.isSafeInteger(target.line) || target.line < 1) continue;
      }
    }
    const end = token[1] ? token.index! + token[0].length - (token[0].endsWith(token[1]) ? 1 : 0) : start + value.length;
    result.push({ ...target, text: value, start, end });
  }
  return result;
}

/** Build a logical wrapped line and map UTF-16 offsets to terminal CELLS. An
 * emoji, wide character or combining mark before a path must not shift its hit
 * target. Bound work even when an application emits a huge unbroken line. */
export function fileLinksForBuffer(buffer: IBuffer, row: number, cols: number): Array<FileLinkMatch & { range: IBufferRange }> {
  let first = row - 1;
  while (first > 0 && buffer.getLine(first)?.isWrapped) {
    if (row - first > 32) return [];
    first--;
  }
  let last = row - 1;
  while (buffer.getLine(last + 1)?.isWrapped) {
    if (last - first >= 32) return [];
    last++;
  }
  let text = "";
  const cells: Array<{ x: number; y: number; width: number }> = [];
  for (let y = first; y <= last; y++) {
    const line = buffer.getLine(y);
    if (!line) break;
    let length = Math.min(cols, line.length);
    // A wide glyph can wrap early, leaving an unoccupied last cell. It is not
    // a space in the printed path. Explicitly printed spaces still stay intact.
    if (y < last) {
      while (length > 0 && line.getCell(length - 1)?.getWidth() === 1 && !line.getCell(length - 1)?.getChars()) length--;
    }
    for (let x = 0; x < length; x++) {
      const cell = line.getCell(x);
      if (!cell || cell.getWidth() === 0) continue;
      const chars = cell.getChars() || " ";
      for (let i = 0; i < chars.length; i++) cells.push({ x: x + 1, y: y + 1, width: cell.getWidth() });
      text += chars;
    }
    if (text.length > 8192) return [];
  }
  return detectFileLinks(text).flatMap((link) => {
    const start = cells[link.start];
    const end = cells[link.end - 1];
    if (!start || !end || row < start.y || row > end.y) return [];
    return [{ ...link, range: { start: { x: start.x, y: start.y }, end: { x: end.x + end.width - 1, y: end.y } } }];
  });
}

export function fileLinkProvider(term: Terminal, handlers: Pick<ILink, "activate" | "hover" | "leave">): ILinkProvider {
  return {
    provideLinks(row, callback) {
      callback(fileLinksForBuffer(term.buffer.active, row, term.cols).map((link) => ({
        ...link,
        ...handlers,
        activate(event) {
          // Python reports its location outside the quoted filename. Preserve
          // that location on activation without decoding/re-encoding other paths.
          const target = link.line !== undefined && parseFileLink(link.text)?.line === undefined
            ? `${link.text}:${link.line}` : link.text;
          handlers.activate(event, target);
        },
      })));
    },
  };
}

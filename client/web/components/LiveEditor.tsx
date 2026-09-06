"use client";

import { useContext, useEffect, useMemo, useRef, useState } from "react";
import CodeMirror from "@uiw/react-codemirror";
import { ReviewToggleContext } from "./reviewContext";
import { EditorView, Decoration, WidgetType, keymap, ViewPlugin } from "@codemirror/view";
import type { DecorationSet, ViewUpdate } from "@codemirror/view";
import { StateField, StateEffect, Facet, Text, type Extension, type Range, type EditorState } from "@codemirror/state";
import { unifiedMergeView, goToNextChunk, goToPreviousChunk, getChunks, rejectChunk, Chunk } from "@codemirror/merge";
import { publishDiffGeometry, type DiffMark } from "@/lib/diffGeometry";
import {
  HighlightStyle,
  syntaxHighlighting,
  syntaxTree,
  LanguageDescription,
  codeFolding,
  foldService,
  foldState,
  foldEffect,
  unfoldEffect,
  foldedRanges,
  foldKeymap,
} from "@codemirror/language";
import { tags as t } from "@lezer/highlight";
import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
import { type MarkdownConfig, type InlineParser, type BlockParser, type Element as MdElement } from "@lezer/markdown";
import { languages } from "@codemirror/language-data";
import { imageDataUrl, writeSkillAsset } from "@/lib/api";
import { log } from "@/lib/log";

// ---------------------------------------------------------------------------
// Syntax highlighting — solid, comprehensive, GitHub-style palette (CSS vars
// so it follows light/dark automatically). Shared by code files and the fenced
// code blocks inside Markdown.
// ---------------------------------------------------------------------------
const codeHighlight = HighlightStyle.define([
  { tag: [t.comment, t.lineComment, t.blockComment, t.docComment], color: "var(--sx-comment)", fontStyle: "italic" },
  { tag: [t.keyword, t.controlKeyword, t.moduleKeyword, t.definitionKeyword, t.operatorKeyword, t.self], color: "var(--sx-keyword)" },
  { tag: [t.string, t.special(t.string), t.docString, t.character, t.attributeValue], color: "var(--sx-string)" },
  { tag: [t.regexp], color: "var(--sx-string)" },
  { tag: [t.number, t.integer, t.float, t.bool, t.null, t.atom, t.unit], color: "var(--sx-number)" },
  { tag: [t.constant(t.variableName), t.standard(t.variableName)], color: "var(--sx-number)" },
  { tag: [t.function(t.variableName), t.function(t.propertyName), t.macroName], color: "var(--sx-function)" },
  { tag: [t.typeName, t.className, t.namespace, t.standard(t.name)], color: "var(--sx-type)" },
  { tag: [t.propertyName, t.attributeName], color: "var(--sx-property)" },
  { tag: [t.tagName], color: "var(--sx-tag)" },
  { tag: [t.meta, t.annotation, t.documentMeta, t.processingInstruction], color: "var(--sx-meta)" },
  { tag: [t.punctuation, t.separator, t.bracket, t.brace, t.paren, t.squareBracket, t.angleBracket], color: "var(--sx-punct)" },
  { tag: [t.operator, t.derefOperator, t.compareOperator, t.arithmeticOperator, t.logicOperator, t.bitwiseOperator], color: "var(--sx-keyword)" },
  { tag: [t.variableName, t.labelName, t.definition(t.variableName)], color: "var(--fg)" },
  { tag: [t.escape, t.special(t.brace)], color: "var(--sx-keyword)" },
  { tag: [t.heading], fontWeight: "700", color: "var(--fg)" },
  { tag: [t.strong], fontWeight: "700" },
  { tag: [t.emphasis], fontStyle: "italic" },
  { tag: t.link, color: "var(--accent)", textDecoration: "underline" },
  { tag: t.url, color: "var(--accent)" },
  { tag: t.strikethrough, textDecoration: "line-through" },
  { tag: t.invalid, color: "var(--error)" },
]);

// ---------------------------------------------------------------------------
// Markdown prose — solid headings/bold, readable-but-secondary markers.
// ---------------------------------------------------------------------------
const markdownHighlight = HighlightStyle.define([
  { tag: t.heading1, fontSize: "1.7em", fontWeight: "700", color: "var(--fg)", lineHeight: "1.3" },
  { tag: t.heading2, fontSize: "1.4em", fontWeight: "700", color: "var(--fg)", lineHeight: "1.3" },
  { tag: t.heading3, fontSize: "1.2em", fontWeight: "700", color: "var(--fg)" },
  { tag: t.heading4, fontSize: "1.05em", fontWeight: "700", color: "var(--fg)" },
  { tag: [t.heading5, t.heading6], fontWeight: "700", color: "var(--fg)" },
  { tag: t.strong, fontWeight: "700", color: "var(--fg)" },
  { tag: t.emphasis, fontStyle: "italic" },
  { tag: t.strikethrough, textDecoration: "line-through" },
  { tag: t.link, color: "var(--accent)", textDecoration: "underline" },
  { tag: t.url, color: "var(--muted)" },
  {
    tag: t.monospace,
    fontFamily: "var(--font-mono)",
    fontSize: "0.9em",
    color: "var(--fg)",
    background: "var(--code-bg)",
    borderRadius: "4px",
    padding: "0.05em 0.3em",
  },
  { tag: t.quote, color: "var(--muted)", fontStyle: "italic" },
  // List item text reads as normal body copy; the bullet/number marker is
  // dimmed separately below (processingInstruction).
  { tag: t.list, color: "var(--fg)" },
  // Markup punctuation (#, **, -, >, link brackets, ---): visible but secondary.
  { tag: t.processingInstruction, color: "var(--mark)" },
  { tag: t.contentSeparator, color: "var(--mark)", fontWeight: "700" },
]);

const baseTheme = EditorView.theme({
  "&": { color: "var(--fg)", backgroundColor: "transparent" },
  ".cm-content": { padding: "0" },
  ".cm-gutters": { display: "none" },
});

// ---------------------------------------------------------------------------
// Live preview: fenced code gets a subtle block background; GFM tables render as
// a real grid when the cursor is outside them, and reveal their (monospace)
// source when you click in to edit — Obsidian Live Preview style.
// ---------------------------------------------------------------------------
const codeLineDeco = Decoration.line({ class: "cm-md-code" });
const fenceOpenDeco = Decoration.line({ class: "cm-md-code cm-md-fence-open" });
const fenceCloseDeco = Decoration.line({ class: "cm-md-code cm-md-fence-close" });
const tableSrcLine = Decoration.line({ class: "cm-md-table" });

// Hide a range entirely (replace with nothing). Used to conceal markup markers.
const hideDeco = Decoration.replace({});
// Marks a rendered link so it shows a pointer cursor (it's clickable).
const linkMarkDeco = Decoration.mark({ class: "cm-md-link" });

// Inline markup markers concealed when the cursor isn't on their line:
//   EmphasisMark **/_  ·  StrikethroughMark ~~  ·  CodeMark ` (inline code only —
//   fenced blocks are handled separately).
const CONCEAL_SIMPLE = new Set(["EmphasisMark", "StrikethroughMark", "CodeMark"]);
// These also swallow trailing whitespace so the rendered text has no leading gap.
const CONCEAL_SPACED = new Set(["HeaderMark", "QuoteMark"]);

function escHtml(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

// Minimal, safe inline-markdown → HTML for table cells (escaped first, then only
// known tags are introduced, so no raw HTML passes through).
function inlineMd(raw: string): string {
  let s = escHtml(raw);
  s = s.replace(/`([^`]+)`/g, (_m, c) => `<code>${c}</code>`);
  s = s.replace(/\*\*([^*]+)\*\*/g, (_m, c) => `<strong>${c}</strong>`);
  s = s.replace(/(^|[^*])\*([^*]+)\*(?!\*)/g, (_m, p, c) => `${p}<em>${c}</em>`);
  s = s.replace(/\[([^\]]+)\]\(([^)]+)\)/g, (_m, t, u) => {
    const url = String(u).trim();
    return /^(https?:|\/|\.|#)/.test(url)
      ? `<a href="${escHtml(url)}" target="_blank" rel="noopener noreferrer">${t}</a>`
      : t;
  });
  return s;
}

function splitRow(line: string): string[] {
  let l = line.trim();
  if (l.startsWith("|")) l = l.slice(1);
  if (l.endsWith("|")) l = l.slice(0, -1);
  return l.split(/(?<!\\)\|/).map((c) => c.trim().replace(/\\\|/g, "|"));
}
function isDelimiterRow(cells: string[]): boolean {
  return cells.length > 0 && cells.every((c) => /^:?-{1,}:?$/.test(c.replace(/\s/g, "")));
}
function alignOf(cell: string): string {
  const c = cell.replace(/\s/g, "");
  const l = c.startsWith(":");
  const r = c.endsWith(":");
  return l && r ? "center" : r ? "right" : l ? "left" : "";
}

// Absolute document range of each cell's trimmed content, for a table-row line
// that starts at `lineStart`. Mirrors splitRow's cell boundaries so a rendered
// cell can be edited in place and written straight back to its source bytes.
function cellRanges(line: string, lineStart: number): { from: number; to: number }[] {
  const ranges: { from: number; to: number }[] = [];
  const n = line.length;
  // Mirror splitRow exactly: trim surrounding whitespace, then strip one leading
  // and one (unescaped) trailing framing pipe before tokenizing — so cell indices
  // line up with the displayed cells even on indented / tab-padded rows.
  let lo = 0;
  let hi = n;
  while (lo < hi && /\s/.test(line[lo])) lo++;
  while (hi > lo && /\s/.test(line[hi - 1])) hi--;
  let k = lo;
  if (k < hi && line[k] === "|") k++;
  let end = hi;
  if (end > k && line[end - 1] === "|" && line[end - 2] !== "\\") end--;
  while (k <= end) {
    let segEnd = k;
    while (segEnd < end && !(line[segEnd] === "|" && line[segEnd - 1] !== "\\")) segEnd++;
    let cs = k;
    let ce = segEnd;
    while (cs < ce && /\s/.test(line[cs])) cs++;
    while (ce > cs && /\s/.test(line[ce - 1])) ce--;
    ranges.push({ from: lineStart + cs, to: lineStart + ce });
    if (segEnd >= end) break;
    k = segEnd + 1;
  }
  return ranges;
}

// Indices of the unescaped `|` separators in a table-row line (framing + interior).
function pipePositions(line: string): number[] {
  const ps: number[] = [];
  for (let i = 0; i < line.length; i++) if (line[i] === "|" && line[i - 1] !== "\\") ps.push(i);
  return ps;
}

class TableWidget extends WidgetType {
  constructor(
    readonly from: number,
    readonly source: string,
    // Whether KaTeX was loaded when this widget was built. Part of eq() so the
    // table re-renders (picking up `$…$` cells) once the engine lands.
    readonly mathReady: boolean,
  ) {
    super();
  }
  eq(other: WidgetType): boolean {
    return (
      other instanceof TableWidget &&
      other.source === this.source &&
      other.from === this.from &&
      other.mathReady === this.mathReady
    );
  }
  ignoreEvent(): boolean {
    return true;
  }
  toDOM(view: EditorView): HTMLElement {
    const from = this.from;
    const source = this.source;
    // Non-blank source rows with their absolute offsets, so each rendered cell
    // knows the exact source range to write back to.
    let off = from;
    const lineInfos: { text: string; start: number }[] = [];
    for (const text of source.split("\n")) {
      if (text.trim()) lineInfos.push({ text, start: off });
      off += text.length + 1;
    }
    const rows = lineInfos.map((li) => splitRow(li.text));
    let header: string[] | null = null;
    let headerInfo: { text: string; start: number } | null = null;
    let aligns: string[] = [];
    let body = rows;
    let bodyInfos = lineInfos;
    if (rows.length >= 2 && isDelimiterRow(rows[1])) {
      header = rows[0];
      headerInfo = lineInfos[0];
      aligns = rows[1].map(alignOf);
      body = rows.slice(2);
      bodyInfos = lineInfos.slice(2);
    }

    // Cells are edited through a floating <input> appended OUTSIDE CodeMirror's
    // content DOM. A nested contentEditable cell leaks keys (Ctrl-A, arrows) to
    // CodeMirror and the browser's editing host; a separate <input> avoids all of
    // that and writes the edited text straight back to that one cell's source
    // range, leaving the rest of the table byte-identical.
    // A <textarea> (not <input>) so a cell whose rendered text wraps onto several
    // lines is editable across the whole cell box rather than a single thin strip.
    const input = document.createElement("textarea");
    input.className = "cm-md-cell-input";
    input.spellcheck = false;
    input.rows = 1;
    input.style.display = "none";
    view.dom.appendChild(input);
    let active: { from: number; to: number; raw: string } | null = null;

    const close = (save: boolean, deferDispatch = false) => {
      const a = active;
      active = null;
      input.style.display = "none";
      window.removeEventListener("scroll", onScroll, true);
      if (!a || !save) return;
      // Keep the table valid: no newlines, and escape any new pipes.
      const next = input.value.replace(/\r?\n/g, " ").replace(/(?<!\\)\|/g, "\\|");
      if (next === a.raw) return;
      const apply = () => view.dispatch({ changes: { from: a.from, to: a.to, insert: next } });
      // Committing re-renders the table (a block widget) and changes its height.
      // If the click that ended the edit lands elsewhere in THIS editor, doing
      // that synchronously reflows the page mid-click, so CodeMirror resolves the
      // click against the post-shrink layout and the caret jumps far from where
      // the user aimed (worst with a big deletion). Defer past the click so it
      // resolves against stable layout; the cell box is already hidden, no flicker.
      if (deferDispatch) requestAnimationFrame(() => view.dom.isConnected && apply());
      else apply();
    };
    // A wide table scrolls horizontally on its own; commit and close if anything
    // scrolls so the box never detaches from its cell.
    const onScroll = () => close(true);

    const openCell = (cell: HTMLElement, range: { from: number; to: number }, raw: string, align: string) => {
      if (!cell.isConnected) return;
      if (active) close(true); // flush any in-progress edit before re-targeting
      active = { from: range.from, to: range.to, raw };
      const cr = cell.getBoundingClientRect();
      const vr = view.dom.getBoundingClientRect();
      input.style.display = "block";
      input.style.left = `${cr.left - vr.left}px`;
      input.style.top = `${cr.top - vr.top}px`;
      input.style.width = `${cr.width}px`;
      input.style.textAlign = align || "left";
      input.value = raw;
      // Grow to fit the (possibly wrapped) content, but never shorter than the cell.
      input.style.height = "auto";
      input.style.height = `${Math.max(cr.height, input.scrollHeight)}px`;
      input.focus({ preventScroll: true }); // a focus-scroll would trip onScroll
      input.select();
      window.addEventListener("scroll", onScroll, true);
    };
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === "Tab") {
        e.preventDefault();
        close(true);
      } else if (e.key === "Escape") {
        e.preventDefault();
        close(false);
      } else if ((e.ctrlKey || e.metaKey) && (e.key === "s" || e.key === "S")) {
        // Commit before ⌘S so the save includes the in-progress cell text.
        e.preventDefault();
        close(true);
      }
    });
    input.addEventListener("blur", (e) => {
      // Defer the commit only when focus stays inside the editor (the "next
      // click" places a caret in the same document — see close()). When focus
      // leaves the editor (switching files/panels), commit now so an unmount
      // can't drop the edit.
      const stay = e.relatedTarget instanceof Node && view.dom.contains(e.relatedTarget);
      close(true, stay);
    });

    const ncols = header ? header.length : rows[0]?.length ?? 0;

    const makeCell = (tag: "td" | "th", display: string, range: { from: number; to: number }, align: string) => {
      const el = document.createElement(tag);
      if (align) el.style.textAlign = align;
      el.className = "cm-md-cell";
      el.innerHTML = renderCellMath(display);
      const raw = source.slice(range.from - from, range.to - from);
      el.addEventListener("mousedown", (e) => {
        e.preventDefault(); // don't let CodeMirror move its own selection
        openCell(el, range, raw, align);
      });
      return el;
    };

    // --- Structural editing. Ops read the LIVE document (so a pending cell edit
    // is reflected) and are surgical — untouched cells stay byte-identical. ---
    const liveLines = (): { text: string; start: number }[] | null => {
      let node = syntaxTree(view.state).resolveInner(from, 1);
      while (node.parent && node.name !== "Table") node = node.parent;
      if (node.name !== "Table") return null;
      const doc = view.state.doc;
      const a = doc.lineAt(node.from);
      const endPos = Math.min(node.to, doc.length);
      const b = doc.lineAt(endPos > a.from ? endPos - 1 : a.from);
      const out: { text: string; start: number }[] = [];
      for (let n = a.number; n <= b.number; n++) {
        const l = doc.line(n);
        if (l.text.trim()) out.push({ text: l.text, start: l.from });
      }
      return out;
    };
    const runOp = (fn: (lines: { text: string; start: number }[]) => void) => {
      if (active) close(true); // commit any in-progress cell edit first
      const lines = liveLines();
      if (lines && lines.length >= 2) fn(lines);
    };
    const blankRow = `|${"  |".repeat(ncols)}`;
    // beforeBody: 0..nbody (nbody = append after the last row).
    const insertRow = (beforeBody: number) =>
      runOp((lines) => {
        if (beforeBody >= lines.length - 2) {
          const last = lines[lines.length - 1];
          view.dispatch({ changes: { from: last.start + last.text.length, insert: `\n${blankRow}` } });
        } else {
          const ln = lines[2 + beforeBody];
          view.dispatch({ changes: { from: ln.start, insert: `${blankRow}\n` } });
        }
      });
    const deleteRow = (bodyIdx: number) =>
      runOp((lines) => {
        const idx = 2 + bodyIdx;
        if (idx < 2 || idx >= lines.length) return;
        const ln = lines[idx];
        if (idx < lines.length - 1) view.dispatch({ changes: { from: ln.start, to: lines[idx + 1].start } });
        else {
          const prev = lines[idx - 1];
          view.dispatch({ changes: { from: prev.start + prev.text.length, to: ln.start + ln.text.length } });
        }
      });
    // beforeCol: 0..ncols (ncols = append on the right). Inserts a cell into
    // every line right after the boundary pipe — only those bytes change.
    const insertCol = (beforeCol: number) =>
      runOp((lines) => {
        const changes: { from: number; insert: string }[] = [];
        lines.forEach((ln, i) => {
          const pipes = pipePositions(ln.text);
          if (pipes.length < beforeCol + 1) return;
          changes.push({ from: ln.start + pipes[beforeCol] + 1, insert: i === 1 ? " --- |" : "  |" });
        });
        if (changes.length) view.dispatch({ changes });
      });
    const deleteCol = (colIdx: number) =>
      runOp((lines) => {
        if (ncols < 2) return;
        const changes: { from: number; to: number }[] = [];
        for (const ln of lines) {
          const pipes = pipePositions(ln.text);
          if (pipes.length < colIdx + 2) continue;
          changes.push({ from: ln.start + pipes[colIdx], to: ln.start + pipes[colIdx + 1] });
        }
        if (changes.length) view.dispatch({ changes });
      });
    // Small gutter control button (delete handle / insert "+").
    const ctrlBtn = (cls: string, title: string, fn: () => void, html?: string) => {
      const b = document.createElement("button");
      b.type = "button";
      b.className = cls;
      b.title = title;
      if (html) b.innerHTML = html;
      b.addEventListener("mousedown", (e) => {
        e.preventDefault();
        e.stopPropagation();
        fn();
      });
      return b;
    };

    const table = document.createElement("table");
    table.className = "cm-md-rendered-table";
    if (header && headerInfo) {
      const ranges = cellRanges(headerInfo.text, headerInfo.start);
      const fallback = { from: headerInfo.start, to: headerInfo.start };
      const thead = document.createElement("thead");
      const tr = document.createElement("tr");
      header.forEach((cell, i) => tr.appendChild(makeCell("th", cell, ranges[i] ?? fallback, aligns[i] ?? "")));
      thead.appendChild(tr);
      table.appendChild(thead);
    }
    const tbody = document.createElement("tbody");
    body.forEach((r, ri) => {
      const info = bodyInfos[ri];
      const ranges = info ? cellRanges(info.text, info.start) : [];
      const fallback = { from: info?.start ?? from, to: info?.start ?? from };
      const tr = document.createElement("tr");
      r.forEach((cell, i) => tr.appendChild(makeCell("td", cell, ranges[i] ?? fallback, aligns[i] ?? "")));
      tbody.appendChild(tr);
    });
    table.appendChild(tbody);

    const scroll = document.createElement("div");
    scroll.className = "cm-md-table-scroll";
    scroll.appendChild(table);

    const wrap = document.createElement("div") as HTMLDivElement & {
      _cellInput?: HTMLTextAreaElement;
      _ro?: ResizeObserver;
    };
    wrap.className = header ? "cm-md-table-wrap cm-tg" : "cm-md-table-wrap";
    wrap._cellInput = input;
    wrap.appendChild(scroll);

    // Notion-style controls overlaid from MEASURED geometry: a "+" bubble that
    // pops up at the row/column boundary nearest the pointer, delete handles in
    // the gutters, and a copy button in the corner. Rebuilt on resize.
    if (header && headerInfo) {
      const overlay = document.createElement("div");
      overlay.className = "cm-tg-overlay";
      wrap.appendChild(overlay);

      let geom: { top: number; bottom: number; left: number; right: number; colBounds: number[]; rowBounds: number[] } | null = null;
      let colIdx = -1;
      let rowIdx = -1;
      let colDels: HTMLElement[] = [];
      let rowDels: HTMLElement[] = [];
      const colPlus = ctrlBtn("cm-tg-plus", "Insert column here", () => {
        if (colIdx >= 0) insertCol(colIdx);
      }, "+");
      const rowPlus = ctrlBtn("cm-tg-plus", "Insert row here", () => {
        if (rowIdx >= 0) insertRow(rowIdx);
      }, "+");

      // A small dropdown so deleting a row/column is a deliberate two-step.
      const menu = document.createElement("div");
      menu.className = "cm-tg-menu";
      menu.style.display = "none";
      wrap.appendChild(menu);
      let docDown: ((ev: MouseEvent) => void) | null = null;
      const closeMenu = () => {
        menu.style.display = "none";
        if (docDown) {
          window.removeEventListener("mousedown", docDown);
          docDown = null;
        }
      };
      const openMenu = (e: MouseEvent, label: string, fn: () => void) => {
        const wr = wrap.getBoundingClientRect();
        const item = document.createElement("button");
        item.type = "button";
        item.className = "cm-tg-menu-item";
        item.innerHTML = `${TRASH_ICON}<span>${label}</span>`;
        item.addEventListener("mousedown", (ev) => {
          ev.preventDefault();
          ev.stopPropagation();
          closeMenu();
          fn();
        });
        menu.replaceChildren(item);
        menu.style.left = `${e.clientX - wr.left}px`;
        menu.style.top = `${e.clientY - wr.top + 6}px`;
        menu.style.display = "block";
        docDown = (ev) => {
          if (!menu.contains(ev.target as Node)) closeMenu();
        };
        setTimeout(() => docDown && window.addEventListener("mousedown", docDown), 0);
      };
      const handle = (cls: string, title: string, label: string, fn: () => void) => {
        const b = document.createElement("button");
        b.type = "button";
        b.className = cls;
        b.title = title;
        b.addEventListener("mousedown", (e) => {
          e.preventDefault();
          e.stopPropagation();
          openMenu(e, label, fn);
        });
        return b;
      };

      const build = () => {
        overlay.replaceChildren();
        colDels = [];
        rowDels = [];
        geom = null;
        const headRow = table.querySelector("thead tr");
        if (!headRow) return;
        const headCells = Array.from(headRow.children) as HTMLElement[];
        if (!headCells.length) return;
        const bodyRows = Array.from(table.querySelectorAll("tbody tr")) as HTMLElement[];
        const wr = wrap.getBoundingClientRect();
        const cols = headCells.map((c) => c.getBoundingClientRect());
        const rrows = bodyRows.map((r) => r.getBoundingClientRect());
        const headR = headRow.getBoundingClientRect();
        const top = headR.top - wr.top;
        const left = cols[0].left - wr.left;
        const right = cols[cols.length - 1].right - wr.left;
        const bottom = (rrows.length ? rrows[rrows.length - 1].bottom : headR.bottom) - wr.top;
        geom = {
          top,
          bottom,
          left,
          right,
          colBounds: [...cols.map((c) => c.left - wr.left), right],
          rowBounds: [...rrows.map((r) => r.top - wr.top), bottom],
        };
        cols.forEach((c, i) => {
          const del = handle("cm-tg-coldel", "Column options", "Delete column", () => deleteCol(i));
          del.style.left = `${c.left - wr.left + 1}px`;
          del.style.width = `${c.width - 2}px`;
          del.style.top = `${top - 9}px`;
          overlay.appendChild(del);
          colDels.push(del);
        });
        rrows.forEach((r, i) => {
          const del = handle("cm-tg-rowdel", "Row options", "Delete row", () => deleteRow(i));
          del.style.top = `${r.top - wr.top + 2}px`;
          del.style.height = `${r.height - 4}px`;
          del.style.left = `${left - 9}px`;
          overlay.appendChild(del);
          rowDels.push(del);
        });
        overlay.appendChild(colPlus);
        overlay.appendChild(rowPlus);
      };

      const onMove = (e: MouseEvent) => {
        if (!geom) return;
        const wr = wrap.getBoundingClientRect();
        const x = e.clientX - wr.left;
        const y = e.clientY - wr.top;
        // "+" bubble at the nearest column boundary
        let cb = -1;
        if (y >= geom.top - 16 && y <= geom.bottom + 6) {
          let d = 7;
          geom.colBounds.forEach((bx, i) => {
            const dd = Math.abs(x - bx);
            if (dd < d) { d = dd; cb = i; }
          });
        }
        colIdx = cb;
        if (cb >= 0) {
          colPlus.style.left = `${geom.colBounds[cb] - 8}px`;
          colPlus.style.top = `${geom.top - 9}px`;
        }
        colPlus.classList.toggle("on", cb >= 0);
        // "+" bubble at the nearest row boundary
        let rb = -1;
        if (x >= geom.left - 16 && x <= geom.right + 6) {
          let d = 7;
          geom.rowBounds.forEach((by, i) => {
            const dd = Math.abs(y - by);
            if (dd < d) { d = dd; rb = i; }
          });
        }
        rowIdx = rb;
        if (rb >= 0) {
          rowPlus.style.top = `${geom.rowBounds[rb] - 8}px`;
          rowPlus.style.left = `${geom.left - 9}px`;
        }
        rowPlus.classList.toggle("on", rb >= 0);
        // only the hovered column's / row's delete handle shows (Notion-style)
        let hc = -1;
        if (y >= geom.top - 12 && y <= geom.bottom && x >= geom.left && x <= geom.right) {
          for (let i = 0; i < colDels.length; i++) {
            if (x >= geom.colBounds[i] && x < geom.colBounds[i + 1]) { hc = i; break; }
          }
        }
        colDels.forEach((el, i) => el.classList.toggle("on", i === hc));
        let hr = -1;
        if (x >= geom.left - 12 && x <= geom.right && y >= geom.top) {
          for (let i = 0; i < rowDels.length; i++) {
            if (y >= geom.rowBounds[i] && y < geom.rowBounds[i + 1]) { hr = i; break; }
          }
        }
        rowDels.forEach((el, i) => el.classList.toggle("on", i === hr));
      };
      wrap.addEventListener("mousemove", onMove);
      wrap.addEventListener("mouseleave", () => {
        colIdx = -1;
        rowIdx = -1;
        colPlus.classList.remove("on");
        rowPlus.classList.remove("on");
        colDels.forEach((el) => el.classList.remove("on"));
        rowDels.forEach((el) => el.classList.remove("on"));
      });

      const ro = new ResizeObserver(() => build());
      ro.observe(table);
      wrap._ro = ro;
    }
    return wrap;
  }
  destroy(dom: HTMLElement): void {
    const d = dom as HTMLElement & { _cellInput?: HTMLTextAreaElement; _ro?: ResizeObserver };
    d._cellInput?.remove();
    d._ro?.disconnect();
  }
}

// Clipboard / check icons for the code-block copy button (SVG renders crisply
// across WebViews; an emoji/text would not).
const COPY_ICON =
  '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>';
const CHECK_ICON =
  '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6 9 17l-5-5"/></svg>';
// Trash icon for the table row/column delete menu.
const TRASH_ICON =
  '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 6h18M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/></svg>';
// Disclosure chevron for the heading-fold toggle. Points down when expanded;
// CSS rotates it -90° (pointing right) when the section is collapsed.
const CHEVRON_ICON =
  '<svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"><path d="m6 9 6 6 6-6"/></svg>';
// Curved "undo" arrow for the per-chunk Revert button (review mode).
const REVERT_ICON =
  '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 14 4 9l5-5"/><path d="M4 9h11a5 5 0 0 1 0 10h-3"/></svg>';

// A small copy button pinned to the top-right of a fenced code block.
class CopyButtonWidget extends WidgetType {
  constructor(readonly code: string) {
    super();
  }
  eq(other: WidgetType): boolean {
    return other instanceof CopyButtonWidget && other.code === this.code;
  }
  ignoreEvent(): boolean {
    return true;
  }
  toDOM(): HTMLElement {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "cm-md-copy";
    btn.innerHTML = COPY_ICON;
    btn.title = "Copy code";
    btn.setAttribute("aria-label", "Copy code");
    btn.addEventListener("mousedown", (e) => e.preventDefault());
    btn.addEventListener("click", (e) => {
      e.preventDefault();
      e.stopPropagation();
      void navigator.clipboard
        ?.writeText(this.code)
        .then(() => {
          btn.innerHTML = CHECK_ICON;
          btn.classList.add("is-copied");
          setTimeout(() => {
            btn.innerHTML = COPY_ICON;
            btn.classList.remove("is-copied");
          }, 1200);
        })
        .catch(() => {});
    });
    return btn;
  }
}

// Track editor focus so tables re-render to a grid once you click away.
const setFocused = StateEffect.define<boolean>();
const focusField = StateField.define<boolean>({
  create: () => false,
  update(val, tr) {
    for (const e of tr.effects) if (e.is(setFocused)) return e.value;
    return val;
  },
});
const focusWatcher = EditorView.domEventHandlers({
  focus: (_e, view) => {
    view.dispatch({ effects: setFocused.of(true) });
  },
  blur: (e, view) => {
    // Only tear down an in-progress reveal when focus genuinely moves to ANOTHER
    // element outside the editor (a link, a button, another input). A click in the
    // editor's own margin/whitespace — or a window switch — blurs to nothing
    // (relatedTarget null); collapsing then would make a centered or wide display-
    // math block impossible to edit, because the click meant to reposition the
    // caret lands just outside the narrow text column and would close the block.
    const fe = e as FocusEvent;
    const leftEditor = fe.relatedTarget instanceof Node && !view.dom.contains(fe.relatedTarget);
    if (leftEditor) view.dispatch({ effects: [setFocused.of(false), setEdit.of(null)] });
  },
});

// Reveal markup for editing only on an explicit double-click — a single click
// just places the caret, so reading/navigating never reflows. `editField` holds
// the block opened for editing; it collapses when the caret leaves it or on blur.
type EditRange = { from: number; to: number } | null;
const setEdit = StateEffect.define<EditRange>();
const editField = StateField.define<EditRange>({
  create: () => null,
  update(val, tr) {
    for (const e of tr.effects) if (e.is(setEdit)) return e.value;
    if (!val) return null;
    let { from, to } = val;
    if (tr.docChanged) {
      from = tr.changes.mapPos(from, -1);
      to = tr.changes.mapPos(to, 1);
    }
    if (tr.selection) {
      const head = tr.state.selection.main.head;
      if (head < from || head > to) return null; // caret left the block → collapse
    }
    return { from, to };
  },
});

// The top-level block (paragraph, heading, table, fenced code, …) containing pos.
function blockRangeAt(state: EditorState, pos: number): { from: number; to: number } {
  const tree = syntaxTree(state);
  // Climb to the top-level block, but stop at a list ITEM so double-clicking one
  // bullet reveals just that item rather than the whole list.
  let node = tree.resolveInner(pos, 1);
  for (let n: typeof node | null = node; n; n = n.parent) {
    if (n.name === "ListItem") {
      node = n;
      break;
    }
    if (!n.parent || n.parent.name === "Document") {
      node = n;
      break;
    }
  }
  if (node.name === "Document") {
    // On a blank line between blocks — just the clicked line.
    const line = state.doc.lineAt(pos);
    return { from: line.from, to: line.to };
  }
  const startLine = state.doc.lineAt(node.from);
  const endPos = Math.min(node.to, state.doc.length);
  const endLine = state.doc.lineAt(endPos > startLine.from ? endPos - 1 : startLine.from);
  return { from: startLine.from, to: endLine.to };
}

// Double-click → reveal the block under the pointer for editing. We do NOT touch
// the selection: the browser's native double-click word-selection stands, so you
// can still double-click to select (and copy) text inside code blocks, etc.
const editGate = EditorView.domEventHandlers({
  dblclick: (event, view) => {
    const pos = view.posAtCoords({ x: event.clientX, y: event.clientY });
    if (pos == null) return false;
    view.dispatch({ effects: setEdit.of(blockRangeAt(view.state, pos)) });
    return false;
  },
});

// ---------------------------------------------------------------------------
// In-page anchor links: a single click on a [text](#heading) link scrolls to
// that heading (GitHub-style slug match). A double-click instead edits the link.
// ---------------------------------------------------------------------------
function slugify(text: string): string {
  return text
    .trim()
    .toLowerCase()
    .replace(/[^\w\s-]/g, "")
    .replace(/\s+/g, "-");
}

function headingPosForSlug(state: EditorState, slug: string): number | null {
  const tree = syntaxTree(state);
  let result: number | null = null;
  tree.iterate({
    enter: (node) => {
      if (result != null) return false;
      if (/^ATXHeading[1-6]$/.test(node.name)) {
        const line = state.doc.lineAt(node.from);
        if (slugify(line.text.replace(/^#+\s*/, "")) === slug) {
          result = node.from;
          return false;
        }
      }
      return undefined;
    },
  });
  return result;
}

// The link under (x, y): an in-page anchor (with its target heading) or an
// external http(s) URL, plus the link's source range. Null if not a link.
type LinkHit =
  | { kind: "anchor"; target: number; from: number; to: number }
  | { kind: "external"; url: string; from: number; to: number };
function anchorAt(view: EditorView, x: number, y: number): LinkHit | null {
  const pos = view.posAtCoords({ x, y });
  if (pos == null) return null;
  let node: ReturnType<typeof syntaxTree>["topNode"] | null = syntaxTree(view.state).resolveInner(pos, 1);
  while (node && node.name !== "Link") node = node.parent;
  if (!node) return null;
  const urlNode = node.getChild("URL");
  if (!urlNode) return null;
  const url = view.state.doc.sliceString(urlNode.from, urlNode.to).trim();
  if (url.startsWith("#")) {
    const target = headingPosForSlug(view.state, slugify(url.slice(1)));
    return target == null ? null : { kind: "anchor", target, from: node.from, to: node.to };
  }
  if (/^https?:\/\//i.test(url)) return { kind: "external", url, from: node.from, to: node.to };
  return null;
}

const anchorNav = (() => {
  let pending: ReturnType<typeof setTimeout> | null = null;
  const cancel = () => {
    if (pending) {
      clearTimeout(pending);
      pending = null;
    }
  };
  return EditorView.domEventHandlers({
    // Only a genuine double-click (edit-the-link) cancels a pending jump — NOT
    // every mousedown, so an impatient re-click never swallows the navigation.
    dblclick: () => {
      cancel();
      return false;
    },
    click: (event, view) => {
      if (event.button !== 0 || event.detail !== 1) return false;
      const hit = anchorAt(view, event.clientX, event.clientY);
      if (!hit) return false;
      const edit = view.state.field(editField, false);
      if (edit && edit.from <= hit.to && edit.to >= hit.from) return false; // editing this link
      cancel();
      pending = setTimeout(() => {
        pending = null;
        if (hit.kind === "anchor")
          view.dispatch({ effects: EditorView.scrollIntoView(hit.target, { y: "start", yMargin: 60 }) });
        else window.open(hit.url, "_blank", "noopener");
      }, 180);
      return false;
    },
  });
})();

// ---------------------------------------------------------------------------
// Collapsible heading sections (Obsidian/Notion-style). A hover chevron in the
// left gutter folds everything under a heading up to the next heading of the
// same-or-higher level. CodeMirror's codeFolding() stores and position-maps the
// folded ranges and renders the "⋯" placeholder; we only compute the section
// extents and dispatch fold/unfold effects.
// ---------------------------------------------------------------------------
type Heading = { level: number; lineFrom: number; foldFrom: number };

// Every heading in document order. `foldFrom` is the position at the end of the
// heading line (where a fold — and its placeholder — begins); for a two-line
// Setext heading that's the end of the underline line.
function headingList(state: EditorState): Heading[] {
  const doc = state.doc;
  const out: Heading[] = [];
  syntaxTree(state).iterate({
    enter: (node) => {
      const m = /^(?:ATX|Setext)Heading([1-6])$/.exec(node.name);
      if (!m) return undefined;
      const first = doc.lineAt(node.from);
      const last = doc.lineAt(Math.min(node.to, doc.length));
      out.push({ level: Number(m[1]), lineFrom: first.from, foldFrom: last.to });
      return false;
    },
  });
  return out;
}

// The collapsible range under heading `idx`: from the end of the heading line to
// the end of the last line before the next heading of the same-or-higher level
// (or the document's end). Null when the section is empty — nothing to collapse.
function sectionRange(state: EditorState, headings: Heading[], idx: number): { from: number; to: number } | null {
  const doc = state.doc;
  const { level, foldFrom } = headings[idx];
  let boundary = 0; // line number of the next sibling/parent heading, if any
  for (let j = idx + 1; j < headings.length; j++) {
    if (headings[j].level <= level) {
      boundary = doc.lineAt(headings[j].lineFrom).number;
      break;
    }
  }
  const to = boundary > 1 ? doc.line(boundary - 1).to : doc.line(doc.lines).to;
  // Empty, or nothing but blank lines under the heading → nothing to collapse.
  if (to <= foldFrom || !doc.sliceString(foldFrom, to).trim()) return null;
  return { from: foldFrom, to };
}

// foldService entry so the keyboard fold commands (foldKeymap) work on headings
// too — given a line, return its section range when that line starts a heading.
const headingFoldService = foldService.of((state, lineStart) => {
  const headings = headingList(state);
  const idx = headings.findIndex((h) => h.lineFrom === lineStart);
  return idx < 0 ? null : sectionRange(state, headings, idx);
});

// The gutter chevron pinned to a heading line. Clicking toggles the section's
// fold; the arrow points down when expanded and right (rotated in CSS) when
// folded. `from`/`to` is the exact range to act on (the live folded range when
// collapsed, so unfoldEffect — which matches on exact bounds — succeeds).
class HeadingFoldWidget extends WidgetType {
  constructor(
    readonly from: number,
    readonly to: number,
    readonly folded: boolean,
  ) {
    super();
  }
  eq(other: WidgetType): boolean {
    return (
      other instanceof HeadingFoldWidget &&
      other.from === this.from &&
      other.to === this.to &&
      other.folded === this.folded
    );
  }
  ignoreEvent(): boolean {
    return true;
  }
  toDOM(view: EditorView): HTMLElement {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "cm-md-fold-btn" + (this.folded ? " is-folded" : "");
    btn.innerHTML = CHEVRON_ICON;
    btn.title = this.folded ? "Expand section" : "Collapse section";
    btn.setAttribute("aria-label", btn.title);
    btn.setAttribute("aria-expanded", this.folded ? "false" : "true");
    btn.addEventListener("mousedown", (e) => e.preventDefault()); // keep the caret put
    btn.addEventListener("click", (e) => {
      e.preventDefault();
      e.stopPropagation();
      view.dispatch({ effects: (this.folded ? unfoldEffect : foldEffect).of({ from: this.from, to: this.to }) });
    });
    return btn;
  }
}

// Custom "⋯" placeholder for a collapsed section (the default click handler
// toggles the fold back open).
const foldPlaceholder = (_view: EditorView, onclick: (event: Event) => void): HTMLElement => {
  const el = document.createElement("span");
  el.className = "cm-md-fold-placeholder";
  el.textContent = "⋯";
  el.title = "Expand section";
  el.setAttribute("aria-label", "Expand section");
  el.onclick = onclick;
  return el;
};

// ---------------------------------------------------------------------------
// Inline images. A local image (`![alt](./logo.png)`) renders as a real <img>
// when its line isn't being edited — mirroring how the editor conceals other
// markup. The bytes come from /api/read-image as a data: URL (the same path
// FilePane uses for standalone images), so it works identically over the
// loopback server and a remote one. External http(s) URLs stay raw: the webview
// CSP only allows self/data:/blob: images, so they could never load anyway.
//
// Decorations build synchronously from state, but the fetch is async, so a
// module cache bridges them: the builder renders only what's already loaded, and
// `imageLoader` fetches misses and dispatches `bumpImages` to trigger one rebuild
// when the data: URL lands. A missing/failed image is left raw so its source
// stays visible and fixable.
// ---------------------------------------------------------------------------

/** Where a markdown file's relative image paths resolve: `root` is the
 *  read-image sandbox root, `dir` is the file's folder within it ("." at root).
 *  null → image rendering off (the raw-source default). */
type ImageCtx = { root: string; dir: string } | null;
const imageCtx = Facet.define<ImageCtx, ImageCtx>({
  combine: (vs) => (vs.length ? vs[vs.length - 1] : null),
});

type ImageEntry = { url: string } | { failed: true };
// Keyed by `${root}\0${rel}`. Module-scoped so it survives editor remounts
// (switching files, entering review) without refetching; never evicted — skills
// hold a handful of small images, so a session won't accumulate many.
const imageCache = new Map<string, ImageEntry>();
const imageInFlight = new Set<string>();
const imageKey = (root: string, rel: string) => `${root}\0${rel}`;

// Recompute decorations when an image finishes loading (the cache changed).
const bumpImages = StateEffect.define<null>();

const IMAGE_EXTS = new Set(["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "svg"]);

/** A markdown image src → its path relative to the asset root, or null when it
 *  shouldn't be fetched (external/absolute URL, or a non-image extension). The
 *  backend re-normalizes and sandboxes; this normalizes too so the cache key is
 *  canonical. */
function resolveImageRel(dir: string, srcRaw: string): string | null {
  let src = srcRaw.trim();
  if (src.startsWith("<") && src.endsWith(">")) src = src.slice(1, -1).trim();
  if (!src) return null;
  // A URL scheme (http:, https:, data:, file:…) or protocol-relative → external.
  if (/^[a-z][a-z0-9+.-]*:/i.test(src) || src.startsWith("//")) return null;
  src = src.replace(/[?#].*$/, ""); // drop any fragment/query the path router can't use
  try {
    src = decodeURIComponent(src); // `my%20pic.png` → the real on-disk `my pic.png`
  } catch {
    /* malformed % escape — fall through with the literal text */
  }
  const ext = src.slice(src.lastIndexOf(".") + 1).toLowerCase();
  if (!IMAGE_EXTS.has(ext)) return null;
  const joined = dir === "." || dir === "" ? src : `${dir}/${src}`;
  const parts: string[] = [];
  for (const seg of joined.split("/")) {
    if (seg === "" || seg === ".") continue;
    if (seg === "..") parts.pop();
    else parts.push(seg);
  }
  return parts.length ? parts.join("/") : null;
}

class ImageWidget extends WidgetType {
  constructor(
    readonly key: string,
    readonly url: string,
    readonly alt: string,
  ) {
    super();
  }
  // key already encodes (root, rel); same key + alt ⇒ identical render, so
  // CodeMirror reuses the DOM (no reload) when an unrelated edit rebuilds.
  eq(other: WidgetType): boolean {
    return other instanceof ImageWidget && other.key === this.key && other.alt === this.alt;
  }
  ignoreEvent(): boolean {
    // false (not the WidgetType default) so a double-click on the image reaches
    // editGate's handler and reveals the raw `![alt](url)` source for editing —
    // the same reveal-on-double-click UX as headings, links, and tables.
    return false;
  }
  toDOM(view: EditorView): HTMLElement {
    const img = document.createElement("img");
    img.className = "cm-md-img";
    img.src = this.url;
    img.alt = this.alt;
    // The data: URL decodes async; once it has real dimensions the line height
    // changes, so ask CodeMirror to re-measure (it can't observe this itself).
    img.addEventListener("load", () => view.requestMeasure());
    return img;
  }
}

// Fetches the document's images and nudges a rebuild as each lands. A ViewPlugin
// (not the builder) because a StateField update must stay pure — no fetch, no
// dispatch.
const imageLoader = ViewPlugin.fromClass(
  class {
    destroyed = false;
    constructor(readonly view: EditorView) {
      this.scan();
    }
    update(u: ViewUpdate) {
      if (u.docChanged || u.startState.facet(imageCtx) !== u.state.facet(imageCtx)) this.scan();
    }
    destroy() {
      this.destroyed = true;
    }
    scan() {
      const ctx = this.view.state.facet(imageCtx);
      if (!ctx) return;
      const doc = this.view.state.doc;
      syntaxTree(this.view.state).iterate({
        enter: (node) => {
          if (node.name !== "Image") return undefined;
          const urlNode = node.node.getChild("URL");
          const rel = urlNode && resolveImageRel(ctx.dir, doc.sliceString(urlNode.from, urlNode.to));
          if (!rel) return false;
          const key = imageKey(ctx.root, rel);
          if (imageCache.has(key) || imageInFlight.has(key)) return false;
          imageInFlight.add(key);
          imageDataUrl(ctx.root, rel)
            .then((url) => imageCache.set(key, { url }))
            .catch(() => imageCache.set(key, { failed: true }))
            .finally(() => {
              imageInFlight.delete(key);
              if (!this.destroyed) this.view.dispatch({ effects: bumpImages.of(null) });
            });
          return false; // don't descend into the image's marks
        },
      });
    }
  },
);

// ---------------------------------------------------------------------------
// Paste / drop media. Dropping or pasting an image into the editor writes the
// bytes into the skill (an `assets/` folder beside the document) via
// /api/write-asset and inserts a `![alt](assets/…)` link at the caret — which the
// inline renderer above then shows as a real <img>, no reload needed. The bytes
// cross to the server (local or remote) the same way a terminal image-paste does.
//
// `assetSink` is the writable target: the skill root + the document's folder
// within it. Set only for markdown that's editable (not review mode); null leaves
// the editor's default text paste untouched.
// ---------------------------------------------------------------------------
type AssetSink = { root: string; dir: string; onWrite?: () => void } | null;
const assetSink = Facet.define<AssetSink, AssetSink>({
  combine: (vs) => (vs.length ? vs[vs.length - 1] : null),
});

const ASSET_SUBDIR = "assets";

// Clipboard image MIME → file extension. Restricted to what the renderer and the
// server's filetypes table both understand; anything else falls through to the
// editor's normal paste (so copied rich text still pastes as text).
const MIME_EXT: Record<string, string> = {
  "image/png": "png",
  "image/jpeg": "jpg",
  "image/gif": "gif",
  "image/webp": "webp",
  "image/bmp": "bmp",
  "image/x-icon": "ico",
  "image/vnd.microsoft.icon": "ico",
  "image/svg+xml": "svg",
};

/** Image files carried by a paste/drop, preferring the explicit file list and
 *  falling back to the item list (some browsers expose a pasted image only there). */
function imageFilesFrom(dt: DataTransfer | null): File[] {
  if (!dt) return [];
  const out: File[] = [];
  for (const f of Array.from(dt.files)) if (f.type.startsWith("image/")) out.push(f);
  if (out.length) return out;
  for (const it of Array.from(dt.items)) {
    if (it.kind === "file" && it.type.startsWith("image/")) {
      const f = it.getAsFile();
      if (f) out.push(f);
    }
  }
  return out;
}

/** A filename to write the media under: a dropped file's own name when it has an
 *  extension, else a synthesized `pasted-image.<ext>` (clipboard images are
 *  usually nameless). The server sanitizes and de-duplicates it. */
function assetName(file: File): string {
  const name = (file.name || "").trim();
  if (name && /\.[a-z0-9]+$/i.test(name)) return name;
  return `pasted-image.${MIME_EXT[file.type] ?? "png"}`;
}

const POSIX_DIR = (dir: string) => dir.replace(/\\/g, "/").replace(/^\.?\/?/, "").replace(/\/+$/, "");

/** The `assets/` directory (relative to the skill root) the media is written to,
 *  for a document sitting in `docDir` within the skill. */
function assetsDirFor(docDir: string): string {
  const d = POSIX_DIR(docDir);
  return d ? `${d}/${ASSET_SUBDIR}` : ASSET_SUBDIR;
}

/** Re-base a root-relative path the server returned to be relative to the
 *  document's own folder, so the link reads `assets/x.png` from any sub-document. */
function docRelative(docDir: string, relFromRoot: string): string {
  const d = POSIX_DIR(docDir);
  if (!d) return relFromRoot;
  const prefix = `${d}/`;
  return relFromRoot.startsWith(prefix) ? relFromRoot.slice(prefix.length) : relFromRoot;
}

/** Alt text from a written path: its basename without the extension. */
function altFromRel(rel: string): string {
  return (rel.split("/").pop() ?? rel).replace(/\.[a-z0-9]+$/i, "");
}

// Each in-flight upload drops a uniquely-tagged placeholder; the upload then
// swaps that exact text for the final link (or a failed marker). A plain string
// find/replace survives unrelated edits elsewhere in the document.
let uploadSeq = 0;

/** Replace the first occurrence of `find` with `replace`, if the view is still
 *  mounted and the placeholder hasn't been edited away. */
function replaceOnce(view: EditorView, find: string, replace: string): void {
  if (!view.dom.isConnected) return;
  const i = view.state.doc.toString().indexOf(find);
  if (i < 0) return;
  try {
    view.dispatch({ changes: { from: i, to: i + find.length, insert: replace } });
  } catch {
    /* view torn down between the read and the dispatch */
  }
}

async function insertMedia(view: EditorView, sink: NonNullable<AssetSink>, files: File[]): Promise<void> {
  let wrote = false;
  for (const file of files) {
    const token = `uploading ${file.name || "image"} ${uploadSeq++}`;
    const placeholder = `![${token}]()`;
    // Drop the placeholder at the caret on its own line, so the spot is visible
    // while the bytes upload and inserts stay in reading order.
    const sel = view.state.selection.main;
    const atLineStart = sel.from === view.state.doc.lineAt(sel.from).from;
    view.dispatch(view.state.replaceSelection(`${atLineStart ? "" : "\n"}${placeholder}\n`));
    try {
      const bytes = new Uint8Array(await file.arrayBuffer());
      const { rel } = await writeSkillAsset(sink.root, assetsDirFor(sink.dir), assetName(file), bytes);
      replaceOnce(view, placeholder, `![${altFromRel(rel)}](${docRelative(sink.dir, rel)})`);
      wrote = true;
    } catch (e) {
      log.debug("editor", "paste media failed", e instanceof Error ? e.message : String(e));
      replaceOnce(view, placeholder, `![upload failed]()`);
    }
  }
  // A new file landed under assets/ — let the host refresh the file tree + the
  // validator's file list so the just-inserted link isn't flagged as missing.
  if (wrote) sink.onWrite?.();
}

const mediaPaste = EditorView.domEventHandlers({
  paste: (event, view) => {
    const sink = view.state.facet(assetSink);
    if (!sink) return false;
    const files = imageFilesFrom(event.clipboardData);
    if (!files.length) return false; // no image → let the normal text paste run
    event.preventDefault();
    void insertMedia(view, sink, files);
    return true;
  },
  // Allow the drop by claiming the dragover when files are in flight (without it
  // the browser would navigate to the dropped file instead of firing `drop`).
  dragover: (event, view) => {
    if (view.state.facet(assetSink) && Array.from(event.dataTransfer?.types ?? []).includes("Files")) {
      event.preventDefault();
    }
    return false;
  },
  drop: (event, view) => {
    const sink = view.state.facet(assetSink);
    if (!sink) return false;
    const files = imageFilesFrom(event.dataTransfer);
    if (!files.length) return false;
    event.preventDefault();
    const pos = view.posAtCoords({ x: event.clientX, y: event.clientY });
    if (pos != null) view.dispatch({ selection: { anchor: pos } });
    void insertMedia(view, sink, files);
    return true;
  },
});

// ---------------------------------------------------------------------------
// Math. `$…$` (inline) and `$$…$$` (block) are parsed into the syntax tree and
// rendered with KaTeX — the same engine VS Code's built-in Markdown preview
// uses. Rendered when idle, revealed as raw `$`-delimited source the moment the
// block is edited (the same conceal/reveal as tables, images, and code).
//
// The `$` delimiter rule is markdown-it-katex's (which VS Code ports), so a bare
// "$5" in prose isn't mistaken for math: an opening `$` may not be followed by
// whitespace, and a closing `$` may not be preceded by whitespace nor followed
// by a digit. Inline math is consumed WHOLE (an eager parser, not a resolve-
// delimiter) so the TeX inside is never re-parsed as markdown (no `_`→emphasis).
// ---------------------------------------------------------------------------
const DOLLAR = 36; // "$"
const BACKSLASH = 92; // "\"
const isMathSpace = (c: number) => c < 0 || c === 32 || c === 9 || c === 10 || c === 13;
const isDigit = (c: number) => c >= 48 && c <= 57;

const inlineMathParser: InlineParser = {
  name: "InlineMath",
  before: "Emphasis", // claim `$` before emphasis so `$a_b$` isn't italicized
  parse(cx, next, pos) {
    // A single `$` only. Both dollars of a `$$` decline: display math is the
    // block parser's job, and declining the SECOND `$` too stops `$$x$$` written
    // mid-paragraph from opening a stray inline span on it.
    if (next !== DOLLAR || cx.char(pos + 1) === DOLLAR || cx.char(pos - 1) === DOLLAR) return -1;
    if (isMathSpace(cx.char(pos + 1))) return -1; // can't open before whitespace
    for (let p = pos + 1; p < cx.end; p++) {
      const c = cx.char(p);
      if (c === BACKSLASH) {
        p++; // TeX escape (`\$`, `\%`, …) — the next char is literal, skip it
        continue;
      }
      if (c === 10) return -1; // inline math stays on one line
      if (c === DOLLAR) {
        // The first unescaped `$` is the ONLY close candidate. If it can't close
        // (preceded by whitespace, or followed by a digit — the currency guard),
        // this isn't math: leave the opening `$` as literal rather than scan on and
        // swallow prose into a bogus span. (markdown-it-katex / VS Code semantics.)
        if (!isMathSpace(cx.char(p - 1)) && !isDigit(cx.char(p + 1))) {
          return cx.addElement(
            cx.elt("InlineMath", pos, p + 1, [
              cx.elt("InlineMathMark", pos, pos + 1),
              cx.elt("InlineMathMark", p, p + 1),
            ]),
          );
        }
        return -1;
      }
    }
    return -1;
  },
};

const blockMathParser: BlockParser = {
  name: "BlockMath",
  parse(cx, line) {
    if (line.next !== DOLLAR || line.text.charCodeAt(line.pos + 1) !== DOLLAR) return false;
    const from = cx.lineStart + line.pos;
    const marks: MdElement[] = [cx.elt("BlockMathMark", from, from + 2)];
    // Closing `$$` later on the SAME line → a one-line `$$ x $$` block, but ONLY
    // when the `$$` ends the (trimmed) line. Trailing text (a period, an equation
    // number) would otherwise be swallowed into the block widget and hidden, so if
    // the line doesn't cleanly end there, don't claim it — leave it as text rather
    // than hide content (or, via the scan below, eat the rest of the document).
    const sameLine = line.text.indexOf("$$", line.pos + 2);
    if (sameLine >= 0) {
      if (line.text.slice(sameLine + 2).trim() !== "") return false;
      const closeFrom = cx.lineStart + sameLine;
      marks.push(cx.elt("BlockMathMark", closeFrom, closeFrom + 2));
      cx.nextLine();
      cx.addElement(cx.elt("BlockMath", from, closeFrom + 2, marks));
      return true;
    }
    // Otherwise scan following lines for one that ends with `$$` (unclosed → EOF,
    // matching how a fenced code block consumes to the end of the document).
    let to = cx.lineStart + line.text.length;
    while (cx.nextLine()) {
      const trimmed = line.text.replace(/\s+$/, "");
      if (trimmed.endsWith("$$")) {
        const closeFrom = cx.lineStart + trimmed.length - 2;
        marks.push(cx.elt("BlockMathMark", closeFrom, closeFrom + 2));
        to = cx.lineStart + trimmed.length;
        cx.nextLine();
        break;
      }
      to = cx.lineStart + line.text.length;
    }
    cx.addElement(cx.elt("BlockMath", from, to, marks));
    return true;
  },
  // A `$$` line ends an open paragraph even without a blank line before it, so a
  // display block written straight under a line of prose is still recognized
  // (matching markdown-it-katex / VS Code) instead of being swallowed as text.
  endLeaf(_cx, line) {
    return line.next === DOLLAR && line.text.charCodeAt(line.pos + 1) === DOLLAR;
  },
};

const mathMarkdownExtension: MarkdownConfig = {
  defineNodes: [
    { name: "BlockMath", block: true },
    { name: "BlockMathMark", style: t.processingInstruction },
    { name: "InlineMath" },
    { name: "InlineMathMark", style: t.processingInstruction },
  ],
  parseBlock: [blockMathParser],
  parseInline: [inlineMathParser],
};

// KaTeX is loaded lazily (it's a few hundred KB + fonts) the first time a
// document actually contains math, so the many math-free files pay nothing.
// Mirrors imageLoader: a module-scoped handle survives editor remounts, and a
// `bumpMath` effect triggers one decoration rebuild once the engine + its CSS
// land. Until then, math stays as raw `$…$` source (a usable fallback).
type Katex = (typeof import("katex"))["default"];
let katexMod: Katex | null = null;
let katexLoading = false;
// Views waiting on the (shared, one-time) lazy KaTeX load. Every mounted editor
// that contains math registers here, so ALL of them rebuild when the engine lands
// — not just the one whose scan kicked off the import. (A remount mid-load would
// otherwise stay raw until its next edit.)
const katexWaiters = new Set<() => void>();
const bumpMath = StateEffect.define<null>();

function requestKatex(bump: () => void): void {
  if (katexMod) {
    bump();
    return;
  }
  katexWaiters.add(bump);
  if (katexLoading) return;
  katexLoading = true;
  Promise.all([import("katex"), import("katex/dist/katex.min.css")])
    .then(([m]) => {
      katexMod = m.default;
    })
    .catch((e) => log.debug("editor", "katex load failed", e instanceof Error ? e.message : String(e)))
    .finally(() => {
      katexLoading = false;
      const waiters = [...katexWaiters];
      katexWaiters.clear();
      for (const w of waiters) w();
    });
}

const mathLoader = ViewPlugin.fromClass(
  class {
    destroyed = false;
    requested = false;
    constructor(readonly view: EditorView) {
      this.scan();
    }
    update(u: ViewUpdate) {
      if (!katexMod && !this.requested && (u.docChanged || syntaxTree(u.startState) !== syntaxTree(u.state))) this.scan();
    }
    destroy() {
      this.destroyed = true;
    }
    scan() {
      if (katexMod || this.requested) return;
      let hasMath = false;
      syntaxTree(this.view.state).iterate({
        enter: (node) => {
          if (node.name === "InlineMath" || node.name === "BlockMath") {
            hasMath = true;
            return false;
          }
          return undefined;
        },
      });
      if (hasMath) {
        this.requested = true;
        requestKatex(() => !this.destroyed && this.view.dispatch({ effects: bumpMath.of(null) }));
      }
    }
  },
);

// Renders one formula. `throwOnError: false` makes KaTeX render a malformed
// formula as inline red source rather than throw; the catch is a final backstop.
// `output: "html"` drops KaTeX's parallel MathML tree (smaller DOM, no
// double-copy of the equation text when selecting).
class MathWidget extends WidgetType {
  constructor(
    readonly tex: string,
    readonly display: boolean,
  ) {
    super();
  }
  eq(other: WidgetType): boolean {
    return other instanceof MathWidget && other.tex === this.tex && other.display === this.display;
  }
  ignoreEvent(): boolean {
    return false; // let a double-click reach editGate and reveal the raw source
  }
  toDOM(): HTMLElement {
    const el = document.createElement(this.display ? "div" : "span");
    el.className = this.display ? "cm-md-math cm-md-math-block" : "cm-md-math cm-md-math-inline";
    if (!katexMod) {
      el.textContent = this.tex; // gated on katexMod, so effectively unreachable
      return el;
    }
    try {
      el.innerHTML = katexMod.renderToString(this.tex, {
        displayMode: this.display,
        throwOnError: false,
        output: "html",
      });
    } catch {
      el.classList.add("cm-md-math-error");
      el.textContent = this.display ? `$$${this.tex}$$` : `$${this.tex}$`;
    }
    return el;
  }
}

// Render one GFM table cell's inline markdown, additionally turning `$…$` spans
// into KaTeX. The TableWidget draws cells through its own inlineMd path (which
// bypasses the syntax tree, and thus the InlineMath parser), so inline math in a
// cell needs handling of its own. Math is split out of the RAW text FIRST, before
// escaping, so the TeX reaches KaTeX intact; the surrounding text keeps the normal
// inlineMd treatment. Same `$` delimiter rule as the inline parser. Falls back to
// plain inlineMd until KaTeX has loaded (raw `$…$` stays visible meanwhile).
function renderCellMath(raw: string): string {
  if (!katexMod) return inlineMd(raw);
  const km = katexMod;
  const out: string[] = [];
  let textStart = 0;
  const flush = (end: number) => {
    if (end > textStart) out.push(inlineMd(raw.slice(textStart, end)));
  };
  for (let i = 0; i < raw.length; i++) {
    // Skip an escaped char so `\$` can't open math (prose gets this for free from
    // the built-in Escape parser; renderCellMath must do it itself).
    if (raw.charCodeAt(i) === BACKSLASH) {
      i++;
      continue;
    }
    // Opening `$`: a single dollar (neither dollar of a `$$`) not before space.
    if (
      raw.charCodeAt(i) !== DOLLAR ||
      raw.charCodeAt(i + 1) === DOLLAR ||
      raw.charCodeAt(i - 1) === DOLLAR ||
      isMathSpace(raw.charCodeAt(i + 1))
    )
      continue;
    let close = -1;
    for (let j = i + 1; j < raw.length; j++) {
      const c = raw.charCodeAt(j);
      if (c === BACKSLASH) {
        j++;
        continue;
      }
      if (c === 10) break;
      if (c === DOLLAR) {
        // First unescaped `$` decides — close if valid, else give up on this span
        // (same rule as the inline parser, so a cell renders like prose).
        if (!isMathSpace(raw.charCodeAt(j - 1)) && !isDigit(raw.charCodeAt(j + 1))) close = j;
        break;
      }
    }
    if (close >= 0) {
      flush(i);
      const tex = raw.slice(i + 1, close);
      try {
        out.push(km.renderToString(tex, { throwOnError: false, output: "html" }));
      } catch {
        out.push(escHtml(`$${tex}$`));
      }
      i = close; // the for-loop's i++ steps past the closing `$`
      textStart = close + 1;
    }
  }
  flush(raw.length);
  return out.join("");
}

function buildMarkdownDecorations(state: EditorState): DecorationSet {
  const doc = state.doc;
  const tree = syntaxTree(state);
  const focused = state.field(focusField, false);
  const edit = state.field(editField, false);
  const decos: Range<Decoration>[] = [];

  // In diff/review mode, the lines inside a changed chunk are revealed as raw
  // markdown (so the diff reads line-by-line); UNCHANGED lines stay fully
  // rendered, exactly like normal viewing. `changedB` holds those chunks' ranges
  // in the current document (the "B" side). Empty when not in diff mode.
  const chunkInfo = getChunks(state);
  const changedB: { from: number; to: number }[] = [];
  if (chunkInfo) {
    for (const c of chunkInfo.chunks) {
      changedB.push({ from: Math.min(c.fromB, doc.length), to: Math.min(c.endB, doc.length) });
    }
  }
  const changedIntersects = (from: number, to: number) =>
    changedB.some((r) => r.from <= to && r.to >= from);

  // Markup is revealed inside the block opened by a double-click (editField, only
  // while focused — a single click just places the caret so reading never
  // reflows) OR inside a diff chunk. `editIntersects` covers multi-line blocks
  // (tables, fences) for both cases.
  const active = focused && edit ? edit : null;
  const editIntersects = (from: number, to: number) =>
    (active != null && active.from <= to && active.to >= from) || changedIntersects(from, to);
  const lineActive = (pos: number) => {
    const line = doc.lineAt(pos);
    return editIntersects(line.from, line.to);
  };

  // Hide a marker, plus any whitespace up to the line's content (for # and >).
  const concealSpaced = (from: number, to: number) => {
    const line = doc.lineAt(from);
    let end = to;
    while (end < line.to && doc.sliceString(end, end + 1) === " ") end++;
    decos.push(hideDeco.range(from, end));
  };

  tree.iterate({
    enter: (node) => {
      const name = node.name;

      // Fenced code: subtle card background; the ``` fences become a faint
      // language label (top) and bottom padding — concealed unless you edit it.
      if (name === "FencedCode") {
        const block = node.node;
        const starts: number[] = [];
        let pos = block.from;
        while (true) {
          const line = doc.lineAt(pos);
          starts.push(line.from);
          if (line.to + 1 > block.to) break;
          pos = line.to + 1;
        }
        starts.forEach((lf, i) => {
          const isOpen = i === 0;
          const isClose = i === starts.length - 1 && starts.length > 1;
          decos.push((isOpen ? fenceOpenDeco : isClose ? fenceCloseDeco : codeLineDeco).range(lf));
        });
        // Copy button pinned to the block's top-right (on the opening fence line).
        const codeNode = block.getChild("CodeText");
        const code = codeNode ? doc.sliceString(codeNode.from, codeNode.to) : "";
        decos.push(
          Decoration.widget({ widget: new CopyButtonWidget(code), side: 1 }).range(doc.lineAt(block.from).to),
        );
        const editing = editIntersects(block.from, block.to);
        if (!editing) {
          for (const m of block.getChildren("CodeMark")) decos.push(hideDeco.range(m.from, m.to));
        }
        return false;
      }

      // Indented code block: just the background.
      if (name === "CodeBlock") {
        let pos = node.from;
        while (true) {
          const line = doc.lineAt(pos);
          decos.push(codeLineDeco.range(line.from));
          if (line.to + 1 > node.to) break;
          pos = line.to + 1;
        }
        return false;
      }

      // GFM table: a real grid when idle, raw monospace source while editing.
      if (name === "Table") {
        const block = node.node;
        const startLine = doc.lineAt(block.from);
        const lastPos = Math.min(block.to, doc.length);
        const endLine = doc.lineAt(lastPos > startLine.from ? lastPos - 1 : startLine.from);
        const editing = editIntersects(startLine.from, endLine.to);
        if (editing) {
          let pos = startLine.from;
          while (true) {
            const line = doc.lineAt(pos);
            decos.push(tableSrcLine.range(line.from));
            if (line.to + 1 > endLine.to) break;
            pos = line.to + 1;
          }
        } else {
          const source = doc.sliceString(startLine.from, endLine.to);
          decos.push(
            Decoration.replace({ widget: new TableWidget(startLine.from, source, !!katexMod), block: true }).range(
              startLine.from,
              endLine.to,
            ),
          );
        }
        return false;
      }

      // Display math: a centered KaTeX block when idle, raw monospace `$$…$$`
      // source while editing (or inside a diff chunk) — same reveal as tables.
      // Rendered only once KaTeX has loaded (mathLoader); until then the source
      // stays visible.
      if (name === "BlockMath") {
        const block = node.node;
        const startLine = doc.lineAt(block.from);
        const lastPos = Math.min(block.to, doc.length);
        const endLine = doc.lineAt(lastPos > startLine.from ? lastPos - 1 : startLine.from);
        if (editIntersects(startLine.from, endLine.to)) {
          let pos = startLine.from;
          while (true) {
            const line = doc.lineAt(pos);
            decos.push(tableSrcLine.range(line.from));
            if (line.to + 1 > endLine.to) break;
            pos = line.to + 1;
          }
        } else if (katexMod) {
          const bm = block.getChildren("BlockMathMark");
          const texFrom = bm.length ? bm[0].to : block.from + 2;
          // No closing mark ⇒ an unclosed block: slice to the block end (there's
          // no closing `$$` to trim, so `block.to - 2` would eat two real chars).
          const texTo = bm.length >= 2 ? bm[bm.length - 1].from : block.to;
          const tex = doc.sliceString(texFrom, texTo);
          decos.push(
            Decoration.replace({ widget: new MathWidget(tex.trim(), true), block: true }).range(startLine.from, endLine.to),
          );
        }
        return false;
      }

      // Inline math: `$…$` → a KaTeX span when idle, raw source while its line is
      // being edited (same reveal as links/inline code).
      if (name === "InlineMath") {
        if (!lineActive(node.from) && katexMod) {
          const im = node.node.getChildren("InlineMathMark");
          const tex =
            im.length >= 2 ? doc.sliceString(im[0].to, im[im.length - 1].from) : doc.sliceString(node.from + 1, node.to - 1);
          decos.push(Decoration.replace({ widget: new MathWidget(tex, false) }).range(node.from, node.to));
        }
        return false;
      }

      // Inline markers — concealed unless their line is being edited.
      if (CONCEAL_SPACED.has(name)) {
        if (!lineActive(node.from)) concealSpaced(node.from, node.to);
        return false;
      }
      if (CONCEAL_SIMPLE.has(name)) {
        if (!lineActive(node.from)) decos.push(hideDeco.range(node.from, node.to));
        return false;
      }
      // Inline image: a real <img> when idle, raw `![alt](url)` while the line is
      // being edited (so the URL stays editable) — same reveal as links/tables.
      // Only images already loaded (see imageLoader) render; anything else (still
      // loading, failed, external, non-image) falls through to raw source.
      if (name === "Image") {
        const ctx = state.facet(imageCtx);
        const urlNode = ctx ? node.node.getChild("URL") : null;
        if (ctx && urlNode && !editIntersects(node.from, node.to)) {
          const rel = resolveImageRel(ctx.dir, doc.sliceString(urlNode.from, urlNode.to));
          const entry = rel ? imageCache.get(imageKey(ctx.root, rel)) : undefined;
          if (rel && entry && "url" in entry) {
            const alt = doc.sliceString(node.from, node.to).replace(/^!\[/, "").replace(/\].*$/s, "");
            decos.push(
              Decoration.replace({ widget: new ImageWidget(imageKey(ctx.root, rel), entry.url, alt) }).range(
                node.from,
                node.to,
              ),
            );
          }
        }
        return false; // marks/URL inside stay raw when not rendered
      }

      // A link gets a pointer cursor (it's clickable) when not being edited.
      if (name === "Link") {
        if (!lineActive(node.from)) decos.push(linkMarkDeco.range(node.from, node.to));
        return undefined; // descend so the marks/URL below still get concealed
      }
      // Links: show just the link text (hide [], (), the URL and any title).
      // Images are left raw — they can't render inline here, so the source is
      // more useful than a mangled fragment.
      if (name === "LinkMark" || name === "URL" || name === "LinkTitle") {
        if (node.node.parent?.name === "Link" && !lineActive(node.from)) {
          decos.push(hideDeco.range(node.from, node.to));
        }
        return false;
      }

      return undefined;
    },
  });

  // Heading-fold chevrons. One per heading that has collapsible content; the
  // heading line is marked relative so the chevron can sit in the left gutter.
  // Skipped entirely when codeFolding() isn't in the config (review/diff mode) —
  // a fold there would hide merge deletion widgets and changed lines.
  const headings = state.field(foldState, false) ? headingList(state) : [];
  if (headings.length) {
    // Assign each live fold to the heading that owns it: the last heading whose
    // foldFrom is ≤ the fold's start. Matching by ownership (not exact `from`
    // equality) keeps the chevron in sync after an edit shifts a fold's start off
    // the heading boundary, while still giving a nested fold to the inner heading.
    const foldByHeading = new Map<number, { from: number; to: number }>();
    foldedRanges(state).between(0, doc.length, (from, to) => {
      let owner = -1;
      for (let i = 0; i < headings.length && headings[i].foldFrom <= from; i++) owner = i;
      if (owner >= 0 && !foldByHeading.has(owner)) foldByHeading.set(owner, { from, to });
    });
    headings.forEach((h, i) => {
      const range = sectionRange(state, headings, i);
      if (!range) return; // empty section — nothing to collapse
      const folded = foldByHeading.get(i);
      const action = folded ?? range;
      decos.push(Decoration.line({ class: "cm-md-heading-line" }).range(h.lineFrom));
      decos.push(
        Decoration.widget({
          widget: new HeadingFoldWidget(action.from, action.to, Boolean(folded)),
          side: -1,
        }).range(h.lineFrom),
      );
    });
  }

  return Decoration.set(decos, true);
}

// Block decorations (the table widgets) must come from a StateField, not a
// ViewPlugin, since they affect layout/height.
const markdownBlocks = StateField.define<DecorationSet>({
  create: (state) => buildMarkdownDecorations(state),
  update(deco, tr) {
    if (
      tr.docChanged ||
      tr.selection ||
      tr.effects.some(
        (e) => e.is(setFocused) || e.is(setEdit) || e.is(foldEffect) || e.is(unfoldEffect) || e.is(bumpImages) || e.is(bumpMath),
      ) ||
      syntaxTree(tr.startState) !== syntaxTree(tr.state) ||
      // The diff chunks finished computing / changed → re-decide which lines to
      // reveal as raw (so unchanged lines render and changed ones show source).
      getChunks(tr.startState)?.chunks !== getChunks(tr.state)?.chunks
    ) {
      return buildMarkdownDecorations(tr.state);
    }
    return deco;
  },
  provide: (f) => EditorView.decorations.from(f),
});

const markdownExtensions: Extension[] = [
  markdown({ base: markdownLanguage, codeLanguages: languages, extensions: mathMarkdownExtension }),
  EditorView.lineWrapping,
  syntaxHighlighting(markdownHighlight),
  syntaxHighlighting(codeHighlight),
  focusField,
  focusWatcher,
  editField,
  editGate,
  anchorNav,
  imageLoader,
  mathLoader,
  mediaPaste,
  markdownBlocks,
  baseTheme,
];

// Collapsible heading sections. Added only to NORMAL markdown editing — kept out
// of review/diff mode, where a fold would hide merge deletion widgets / changed
// lines (and buildMarkdownDecorations omits the chevrons when foldState is absent).
const headingFolding: Extension[] = [
  codeFolding({ placeholderDOM: foldPlaceholder }),
  headingFoldService,
  keymap.of(foldKeymap),
];

const mergeNavKeymap = keymap.of([
  { key: "F7", run: goToNextChunk },
  { key: "Shift-F7", run: goToPreviousChunk },
]);

// Map merge chunks to overview marks via CodeMirror's height map (handles
// off-screen, wrapped, and widget-displaced lines). Used by the change tracker.
function chunkMarks(view: EditorView, chunks: readonly Chunk[]): DiffMark[] {
  const pad = view.documentPadding;
  const docLen = view.state.doc.length;
  const out: DiffMark[] = [];
  for (const c of chunks) {
    const fromB = Math.min(c.fromB, docLen);
    const endB = Math.min(c.endB, docLen);
    // empty B ⇒ lines removed (del); empty A (nothing in baseline) ⇒ pure
    // addition (add); content on both sides ⇒ modification (mod).
    const emptyB = fromB === endB;
    const kind = emptyB ? "del" : c.fromA === c.endA ? "add" : "mod";
    const topBlk = view.lineBlockAt(fromB);
    // endB is the start of the line AFTER the change; step back one line.
    const botBlk = view.lineBlockAt(emptyB ? fromB : Math.max(fromB, endB - 1));
    out.push({ top: pad.top + topBlk.top, height: Math.max(botBlk.bottom - topBlk.top, 2), kind, pos: fromB });
  }
  return out;
}

// ---------------------------------------------------------------------------
// Change tracker — used in NORMAL editing (no merge overlay): diffs the live
// buffer against the HEAD baseline on every edit and publishes the changed
// chunks' geometry, so the same overview ruler + left change bars that review
// mode shows also appear while editing (VS Code "dirty diff" style). A fresh
// field/plugin per baseline keeps it config-driven (reconfigure on baseline
// change only — never on a keystroke).
// ---------------------------------------------------------------------------
// Per-chunk Revert button (review mode only): a small undo button in the left
// gutter that drops the chunk back to its committed version. Only shown while the
// merge overlay is active (review), where rejectChunk has the original to restore.
class RevertWidget extends WidgetType {
  constructor(readonly pos: number) {
    super();
  }
  eq(other: WidgetType): boolean {
    return other instanceof RevertWidget && other.pos === this.pos;
  }
  ignoreEvent(): boolean {
    return true;
  }
  toDOM(view: EditorView): HTMLElement {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "cm-diff-revert";
    btn.title = "Revert this change to the committed version";
    btn.setAttribute("aria-label", "Revert this change");
    btn.innerHTML = REVERT_ICON;
    btn.addEventListener("mousedown", (e) => e.preventDefault());
    btn.addEventListener("click", (e) => {
      e.preventDefault();
      e.stopPropagation();
      rejectChunk(view, this.pos);
    });
    return btn;
  }
}

// Which chunk the pointer is over, so the WHOLE chunk's bar reacts to hover as one
// block (the bar is rendered per-line — the only way it scrolls correctly inside
// CodeMirror — so a shared hover state is what makes the segments read as a single
// bar). Set by the gutter mousemove handler in changeTracker.
const setHoveredChunk = StateEffect.define<{ from: number; to: number } | null>();
const hoveredChunk = StateField.define<{ from: number; to: number } | null>({
  create: () => null,
  update(val, tr) {
    for (const e of tr.effects) if (e.is(setHoveredChunk)) return e.value;
    if (val && tr.docChanged) return { from: tr.changes.mapPos(val.from, -1), to: tr.changes.mapPos(val.to, 1) };
    return val;
  },
});

// The change bar itself: a thin green/yellow/red bar in the left gutter, one per
// changed line. A real widget element (not a ::before) so it has its own :hover
// (the gutter sits outside the line's box, so a pseudo-element there can't drive
// the line's :hover) and handles its own click → toggle "Review changes".
type DiffKind = "add" | "mod" | "del";
class DiffBarWidget extends WidgetType {
  constructor(
    readonly kind: DiffKind,
    readonly from: number, // the chunk's B-range, so hovering any segment lights the whole bar
    readonly to: number,
    readonly onReview?: () => void,
  ) {
    super();
  }
  eq(other: WidgetType): boolean {
    return (
      other instanceof DiffBarWidget &&
      other.kind === this.kind &&
      other.from === this.from &&
      other.to === this.to &&
      !!other.onReview === !!this.onReview
    );
  }
  ignoreEvent(): boolean {
    return true;
  }
  toDOM(view: EditorView): HTMLElement {
    const el = document.createElement("div");
    el.className = `cm-diff-bar cm-diff-${this.kind}`;
    const range = { from: this.from, to: this.to };
    // Hover any line's segment → the whole chunk's bar lights up (native enter/
    // leave still fire even though ignoreEvent() hides it from CodeMirror).
    el.addEventListener("mouseenter", () => {
      const cur = view.state.field(hoveredChunk, false);
      if (!cur || cur.from !== range.from || cur.to !== range.to) view.dispatch({ effects: setHoveredChunk.of(range) });
    });
    el.addEventListener("mouseleave", (e) => {
      const to = e.relatedTarget as HTMLElement | null;
      if (to?.closest?.(".cm-diff-bar")) return; // moving onto another segment of a bar
      if (view.state.field(hoveredChunk, false)) view.dispatch({ effects: setHoveredChunk.of(null) });
    });
    if (this.onReview) {
      const review = this.onReview;
      el.title = "Review changes since the last commit";
      el.setAttribute("role", "button");
      el.addEventListener("mousedown", (e) => e.preventDefault());
      el.addEventListener("click", (e) => {
        e.preventDefault();
        e.stopPropagation();
        review();
      });
    }
    return el;
  }
}

// The in-editor change chrome: a DiffBarWidget on every changed line (VS Code
// "dirty diff", live as you edit), plus a Revert button per chunk in review mode.
// Rendered as CodeMirror line/widget decorations so it tracks scroll, wrapping and
// virtualization for free and stays INSIDE the editor column. The overview ruler
// on the scroll pane is the only diff chrome that stays an overlay (the native
// scrollbar can't be decorated from here).
function buildDiffMarkers(
  state: EditorState,
  chunks: readonly Chunk[],
  review: boolean,
  onReview?: () => void,
): DecorationSet {
  const doc = state.doc;
  const hovered = state.field(hoveredChunk, false) ?? null;
  const decos: Range<Decoration>[] = [];
  for (const c of chunks) {
    const fromB = Math.min(c.fromB, doc.length);
    const endB = Math.min(c.endB, doc.length);
    // empty B ⇒ lines removed (del); empty A ⇒ pure addition (add); both ⇒ mod.
    const emptyB = fromB === endB;
    const kind: DiffKind = emptyB ? "del" : c.fromA === c.endA ? "add" : "mod";
    const startLine = doc.lineAt(fromB);
    const lastLine = emptyB ? startLine : doc.lineAt(endB > fromB ? endB - 1 : fromB);
    // Whole chunk lights up when the pointer is over any of its lines.
    const isHovered = !!hovered && hovered.from <= endB && hovered.to >= fromB;
    const lineClass = isHovered ? "cm-diff-line cm-diff-hover" : "cm-diff-line";
    for (let n = startLine.number; n <= lastLine.number; n++) {
      const ln = doc.line(n);
      decos.push(Decoration.line({ class: lineClass }).range(ln.from)); // position:relative anchor
      decos.push(Decoration.widget({ widget: new DiffBarWidget(kind, fromB, endB, onReview), side: -1 }).range(ln.from));
    }
    if (review) decos.push(Decoration.widget({ widget: new RevertWidget(fromB), side: -1 }).range(startLine.from));
  }
  return Decoration.set(decos, true);
}

function changeTracker(baseline: string, review: boolean, onReview?: () => void): Extension[] {
  const baseText = Text.of(baseline.length ? baseline.split("\n") : [""]);
  const chunkField = StateField.define<readonly Chunk[]>({
    create: (state) => Chunk.build(baseText, state.doc),
    update: (chunks, tr) => (tr.docChanged ? Chunk.build(baseText, tr.state.doc) : chunks),
  });
  // Left-gutter change bars + (review) revert buttons, rebuilt when the chunks
  // change. Block/line decorations, so they live in the document flow.
  const markers = StateField.define<DecorationSet>({
    // `field(chunkField, false)` (no throw) so a reconfigure transaction — which
    // swaps in fresh field instances when entering/leaving review — never trips
    // "field not present" before chunkField is initialized in the new state.
    create: (state) => {
      const chunks = state.field(chunkField, false);
      return chunks ? buildDiffMarkers(state, chunks, review, onReview) : Decoration.none;
    },
    update(deco, tr) {
      const prev = tr.startState.field(chunkField, false);
      const next = tr.state.field(chunkField, false);
      const hoverChanged = tr.effects.some((e) => e.is(setHoveredChunk));
      if (tr.docChanged || prev !== next || hoverChanged) {
        return next ? buildDiffMarkers(tr.state, next, review, onReview) : Decoration.none;
      }
      return deco;
    },
    provide: (f) => EditorView.decorations.from(f),
  });
  const reporter = ViewPlugin.fromClass(
    class {
      view: EditorView;
      lastSig = "";
      constructor(view: EditorView) {
        this.view = view;
        this.schedule();
      }
      update(u: ViewUpdate) {
        if (u.docChanged || u.geometryChanged || u.startState.field(chunkField) !== u.state.field(chunkField)) {
          this.schedule();
        }
      }
      schedule() {
        this.view.requestMeasure<DiffMark[]>({
          read: () => chunkMarks(this.view, this.view.state.field(chunkField)),
          write: (marks) => {
            // Skip republishing (and the React re-render) when nothing moved.
            const sig = marks.map((m) => `${Math.round(m.top)}:${Math.round(m.height)}:${m.kind}`).join("|");
            if (sig === this.lastSig) return;
            this.lastSig = sig;
            publishDiffGeometry({ el: this.view.dom, marks });
          },
        });
      }
      destroy() {
        publishDiffGeometry(null);
      }
    },
  );
  // hoveredChunk holds the chunk under the pointer (set by the bar widgets' own
  // hover listeners), so every line of that chunk lights its bar as one block.
  return [chunkField, hoveredChunk, markers, reporter];
}


/** Find a CodeMirror language for a file by my language id or its filename. */
function matchLanguage(language?: string, filename?: string): LanguageDescription | null {
  if (language) {
    const byName = LanguageDescription.matchLanguageName(languages, language, true);
    if (byName) return byName;
  }
  if (filename) {
    const byFile = LanguageDescription.matchFilename(languages, filename);
    if (byFile) return byFile;
  }
  return null;
}

export default function LiveEditor({
  value,
  onChange,
  kind,
  language,
  filename,
  placeholder,
  baseline,
  review,
  assets,
  onAsset,
}: {
  value: string;
  onChange: (value: string) => void;
  kind: "markdown" | "code";
  language?: string;
  filename?: string;
  placeholder?: string;
  /** Markdown only: where relative image paths resolve, so `![alt](./x.png)`
   *  renders inline. `root` is the read-image sandbox; `dir` is the file's folder
   *  within it ("." when the file sits at the root). Omitted → images stay raw.
   *  When present (and not in review), it also enables paste/drop-to-insert media. */
  assets?: { root: string; dir: string };
  /** Markdown only: called after a pasted/dropped image is written into the skill,
   *  so the host can refresh the file tree + the validator's known-files list (the
   *  new asset would otherwise read as a missing reference until reload). */
  onAsset?: () => void;
  /** The file's HEAD content. When defined (a tracked file), live change
   *  indicators (overview ruler + left red/green bars) track edits against it.
   *  "" = a new/untracked file (whole buffer reads as added). undefined = not
   *  tracked → no indicators. */
  baseline?: string;
  /** Markdown only: also render the inline review overlay (deletions shown above
   *  changed lines, changed lines revealed as raw). Needs `baseline`. */
  review?: boolean;
}) {
  // Lazily load the right language support for code files (covers ~140 langs).
  const [codeLang, setCodeLang] = useState<Extension[]>([]);
  useEffect(() => {
    if (kind !== "code") return;
    let cancelled = false;
    const desc = matchLanguage(language, filename);
    // No reset for the no-match case: each file remounts the editor (keyed by
    // rel), so codeLang already starts empty.
    desc?.load().then((support) => {
      if (!cancelled) setCodeLang([support]);
    });
    return () => {
      cancelled = true;
    };
  }, [kind, language, filename]);

  // "Review changes" toggle from StudioLayout, behind a stable wrapper so a fresh
  // toggle identity each render never reconfigures the editor; the gutter change
  // bars call it on click.
  const reviewToggle = useContext(ReviewToggleContext);
  const reviewToggleRef = useRef(reviewToggle);
  reviewToggleRef.current = reviewToggle;
  const onReview = useMemo(() => () => reviewToggleRef.current?.(), []);

  // Same stable-wrapper trick for the asset-written callback: a fresh `onAsset`
  // identity each render must not reconfigure the editor (it rides in a facet).
  const onAssetRef = useRef(onAsset);
  onAssetRef.current = onAsset;
  const onAssetStable = useMemo(() => () => onAssetRef.current?.(), []);

  // Primitives (not the fresh `assets` object) so the extension memo is stable.
  const assetRoot = assets?.root;
  const assetDir = assets?.dir ?? ".";

  // Memoized so editing (which changes `value`, NOT these deps) never rebuilds
  // the extension array — only a baseline reload or entering review reconfigures
  // (rare events), which @uiw applies via StateEffect.reconfigure, preserving the
  // document. REQUIRES @codemirror/state >= 6.5.2.
  const extensions = useMemo<Extension[]>(() => {
    // The base is identical to normal editing — for markdown the full WYSIWYG
    // (markdownBlocks reveals raw source only inside changed chunks while review
    // is on; untouched content always looks like normal viewing).
    const base =
      kind === "markdown"
        ? [
            ...markdownExtensions,
            // Resolve relative image paths against the file's folder so they render
            // inline; absent → images stay raw (markdownExtensions' default).
            ...(assetRoot ? [imageCtx.of({ root: assetRoot, dir: assetDir })] : []),
            // Enable paste/drop-to-insert media — but not in review mode, where the
            // document is being read against a diff, not authored.
            ...(assetRoot && !review ? [assetSink.of({ root: assetRoot, dir: assetDir, onWrite: onAssetStable })] : []),
            // Folding is off in review mode (it would hide diff content).
            ...(review ? [] : headingFolding),
          ]
        : [...codeLang, EditorView.lineWrapping, syntaxHighlighting(codeHighlight), baseTheme];
    if (baseline === undefined) return base; // not tracked → plain editor
    // Live change indicators (in-editor gutter bars + overview ruler) for every
    // tracked file; the per-chunk Revert button is added in review mode.
    const exts: Extension[] = [...base, ...changeTracker(baseline, Boolean(review) && kind === "markdown", onReview)];
    // The inline review overlay is markdown-only (code reviews via a read-only
    // diff in the file pane).
    if (review && kind === "markdown") {
      exts.push(
        mergeNavKeymap,
        ...unifiedMergeView({
          original: baseline,
          gutter: false, // indicators live in the margins, not an in-editor gutter
          highlightChanges: true,
          syntaxHighlightDeletions: true,
          // Full-line (not inline) deletions: an inline <del> renders at base size
          // and clashes with heading-sized text; a full deleted line is syntax-
          // highlighted to match its replacement.
          allowInlineDiffs: false,
          mergeControls: false, // revert lives in the left margin overlay
        }),
      );
    }
    return exts;
  }, [kind, codeLang, baseline, review, onReview, assetRoot, assetDir, onAssetStable]);

  return (
    <CodeMirror
      value={value}
      onChange={onChange}
      placeholder={placeholder}
      extensions={extensions}
      className={kind === "code" ? "cm-mono" : "cm-prose"}
      basicSetup={{
        lineNumbers: false,
        foldGutter: false,
        highlightActiveLine: false,
        highlightActiveLineGutter: false,
        autocompletion: false,
        bracketMatching: false,
        closeBrackets: false,
        highlightSelectionMatches: false,
        searchKeymap: false,
        indentOnInput: false,
        // Use the browser's native selection: CodeMirror's drawn selection sits
        // BEHIND the line backgrounds, so it's invisible inside code blocks.
        drawSelection: false,
      }}
    />
  );
}

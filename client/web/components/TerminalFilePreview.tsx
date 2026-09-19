"use client";

import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import CodeMirror from "@uiw/react-codemirror";
import { EditorView } from "@codemirror/view";
import { HighlightStyle, LanguageDescription, syntaxHighlighting } from "@codemirror/language";
import { languages } from "@codemirror/language-data";
import type { Extension } from "@codemirror/state";
import { tags } from "@lezer/highlight";
import { Modal } from "./Modal";
import { btnGhost } from "./ui";
import * as api from "@/lib/api";
import { humanSize } from "@/lib/fileTypes";
import type { FileData } from "@/lib/types";

interface LinkedFile {
  path: string;
  root: string;
  rel: string;
  line?: number;
  column?: number;
}

const previewTheme = EditorView.theme({
  "&": { height: "100%", backgroundColor: "var(--surface)", color: "var(--fg)" },
  ".cm-scroller": { overflow: "auto", fontFamily: "var(--font-mono)", fontSize: "13px" },
  ".cm-content": { padding: "12px 0", caretColor: "var(--fg)" },
  ".cm-gutters": { backgroundColor: "var(--panel)", color: "var(--faint)", borderColor: "var(--border)" },
  ".cm-activeLine, .cm-activeLineGutter": { backgroundColor: "var(--accent-soft)" },
  "&.cm-focused .cm-selectionBackground, .cm-selectionBackground": { backgroundColor: "var(--accent-soft)" },
  "&.cm-focused": { outline: "none" },
});

const previewHighlight = syntaxHighlighting(HighlightStyle.define([
  { tag: tags.comment, color: "var(--sx-comment)", fontStyle: "italic" },
  { tag: [tags.keyword, tags.operator], color: "var(--sx-keyword)" },
  { tag: [tags.string, tags.regexp], color: "var(--sx-string)" },
  { tag: [tags.number, tags.bool, tags.null], color: "var(--sx-number)" },
  { tag: [tags.function(tags.variableName), tags.function(tags.propertyName)], color: "var(--sx-function)" },
  { tag: [tags.typeName, tags.className], color: "var(--sx-type)" },
  { tag: [tags.propertyName, tags.attributeName], color: "var(--sx-property)" },
  { tag: tags.tagName, color: "var(--sx-tag)" },
  { tag: tags.meta, color: "var(--sx-meta)" },
  { tag: tags.punctuation, color: "var(--sx-punct)" },
  { tag: tags.heading, fontWeight: "bold" },
  { tag: tags.strong, fontWeight: "bold" },
  { tag: tags.emphasis, fontStyle: "italic" },
  { tag: [tags.link, tags.url], color: "var(--accent)" },
]));

function TextPreview({ data, file }: { data: FileData; file: LinkedFile }) {
  const [language, setLanguage] = useState<Extension[]>([]);
  useEffect(() => {
    let alive = true;
    setLanguage([]);
    const match = LanguageDescription.matchLanguageName(languages, data.language, true)
      ?? LanguageDescription.matchFilename(languages, file.rel);
    match?.load().then(
      (support) => { if (alive) setLanguage([support]); },
      () => {}, // A missing grammar still leaves a useful plain-text preview.
    );
    return () => { alive = false; };
  }, [data.language, file.rel]);

  const extensions = useMemo(() => [
    previewTheme,
    previewHighlight,
    EditorView.contentAttributes.of({ "aria-label": file.rel, "aria-readonly": "true" }),
    ...language,
  ], [file.rel, language]);

  const revealLocation = (view: EditorView) => {
    const requestedLine = Number.isFinite(file.line) ? Math.trunc(file.line!) : 1;
    const line = view.state.doc.line(Math.max(1, Math.min(requestedLine, view.state.doc.lines)));
    const requestedColumn = Number.isFinite(file.column) ? Math.max(1, Math.trunc(file.column!)) : undefined;
    const anchor = requestedColumn === undefined ? line.from : Math.min(line.to, line.from + requestedColumn - 1);
    view.dispatch({
      selection: { anchor, head: requestedColumn === undefined ? line.to : anchor },
      effects: EditorView.scrollIntoView(anchor, { y: "center", x: "nearest" }),
    });
  };

  return (
    <CodeMirror
      value={data.content ?? ""}
      readOnly
      extensions={extensions}
      height="100%"
      className="h-full min-h-0"
      onCreateEditor={(view) => {
        revealLocation(view);
      }}
      basicSetup={{
        lineNumbers: true,
        foldGutter: false,
        highlightActiveLine: true,
        highlightActiveLineGutter: true,
        autocompletion: false,
        closeBrackets: false,
        indentOnInput: false,
        highlightSelectionMatches: false,
      }}
    />
  );
}

/** A terminal link opens a read-only view on the current server without leaving its session. */
export default function TerminalFilePreview({ file, onClose }: { file: LinkedFile; onClose: () => void }) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const titleId = useId();
  const pathId = useId();
  const fileKey = `${file.root}\0${file.rel}`;
  const [loaded, setLoaded] = useState<{ key: string; data?: FileData; image?: string; error?: string } | null>(null);
  const [editor, setEditor] = useState<{ available: boolean; name?: string } | null>(null);
  const [editorError, setEditorError] = useState<string | null>(null);
  const [openingEditor, setOpeningEditor] = useState(false);
  const mountedRef = useRef(true);
  const current = loaded?.key === fileKey ? loaded : null;
  const data = current?.data;
  const filename = file.rel.split(/[\\/]/).pop() || file.rel;

  useEffect(() => {
    mountedRef.current = true;
    return () => { mountedRef.current = false; };
  }, []);

  useEffect(() => {
    let alive = true;
    setLoaded(null);
    setEditorError(null);
    const load = async () => {
      try {
        const data = await api.readFile(file.root, file.rel);
        if (!alive) return;
        // Avoid reading arbitrarily large image files into a base64 JSON response.
        const oversizedImage = data.category === "image" && data.size > 20 * 1024 * 1024;
        const image = data.category === "image" && !oversizedImage
          ? await api.imageDataUrl(file.root, file.rel) : undefined;
        if (alive) setLoaded({ key: fileKey, data: oversizedImage ? { ...data, tooLarge: true } : data, image });
      } catch (error) {
        if (alive) setLoaded({ key: fileKey, error: error instanceof Error ? error.message : String(error) });
      }
    };
    void load();
    return () => { alive = false; };
  }, [file.root, file.rel, fileKey]);

  useEffect(() => {
    let alive = true;
    api.editorStatus().then(
      (status) => { if (alive) setEditor(status); },
      () => { if (alive) setEditor(null); },
    );
    return () => { alive = false; };
  }, []);

  useLayoutEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    const focusDialog = () => (dialog.querySelector<HTMLButtonElement>("button") ?? dialog).focus();
    focusDialog();
    // Terminal resize/attach handlers may try to reclaim focus while the file is open.
    const containFocus = (event: FocusEvent) => {
      if (event.target instanceof Node && !dialog.contains(event.target)) focusDialog();
    };
    document.addEventListener("focusin", containFocus);
    return () => document.removeEventListener("focusin", containFocus);
  }, []);

  const openEditor = async () => {
    setEditorError(null);
    setOpeningEditor(true);
    try {
      await api.editorOpen(file.path);
    } catch (error) {
      if (mountedRef.current) setEditorError(error instanceof Error ? error.message : String(error));
    } finally {
      if (mountedRef.current) setOpeningEditor(false);
    }
  };

  return createPortal(
    <div
      ref={dialogRef}
      role="dialog"
      aria-modal="true"
      aria-labelledby={titleId}
      aria-describedby={pathId}
      tabIndex={-1}
      onPointerDown={(event) => event.stopPropagation()}
      onClick={(event) => event.stopPropagation()}
      onKeyDown={(event) => event.stopPropagation()}
      onKeyDownCapture={(event) => {
        if (event.key === "Escape") {
          event.preventDefault();
          event.stopPropagation();
          onClose();
        } else if (event.key === "Tab") {
          const focusable = Array.from(event.currentTarget.querySelectorAll<HTMLElement>(
            'button:not(:disabled), a[href], input:not(:disabled), [tabindex="0"], [contenteditable="true"]',
          )).filter((element) => element.getClientRects().length > 0);
          const first = focusable[0];
          const last = focusable[focusable.length - 1];
          if (event.shiftKey && (document.activeElement === first || document.activeElement === event.currentTarget)) {
            event.preventDefault();
            last?.focus();
          } else if (!event.shiftKey && document.activeElement === last) {
            event.preventDefault();
            first?.focus();
          }
        }
      }}
    >
      <Modal title={<span id={titleId}>{filename}</span>} onClose={onClose} widthClass="max-w-5xl">
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-border px-5 py-2 text-xs text-muted">
          <span id={pathId} className="min-w-0 break-all font-mono">{file.path}</span>
          {file.line !== undefined && <span>Line {file.line}{file.column !== undefined ? `, column ${file.column}` : ""}</span>}
          {data && <span>{humanSize(data.size)}</span>}
          <span className="ml-auto shrink-0">Read only</span>
        </div>
        <div className="h-[min(65dvh,44rem)] min-h-40 overflow-auto" aria-busy={!current}>
          {!current ? (
            <p role="status" className="p-5 text-sm text-muted">Loading file…</p>
          ) : current.error ? (
            <p role="alert" className="p-5 text-sm text-danger">Could not open file: {current.error}</p>
          ) : data?.tooLarge ? (
            <p className="p-5 text-sm text-muted">File is too large to preview ({humanSize(data.size)}).</p>
          ) : data?.isBinary || data?.category === "binary" ? (
            <p className="p-5 text-sm text-muted">Binary file — preview is not available.</p>
          ) : current.image ? (
            <div className="flex min-h-full items-center justify-center p-5">
              <img src={current.image} alt={filename} className="max-h-full max-w-full object-contain" onError={() => {
                setLoaded({ key: fileKey, data, error: "The image format could not be displayed." });
              }} />
            </div>
          ) : data?.content !== undefined ? (
            <TextPreview key={`${fileKey}:${file.line}:${file.column}`} data={data} file={file} />
          ) : (
            <p className="p-5 text-sm text-muted">Preview is not available for this file.</p>
          )}
        </div>
        {editor?.available && (
          <div className="flex flex-wrap items-center gap-3 border-t border-border px-5 py-3">
            {editorError && <span role="alert" className="min-w-0 flex-1 text-sm text-danger">{editorError}</span>}
            <button type="button" className={`${btnGhost} ml-auto`} onClick={() => void openEditor()} disabled={openingEditor}>
              {openingEditor ? "Opening…" : `Open in ${editor.name ?? "VS Code"}`}
            </button>
          </div>
        )}
      </Modal>
    </div>,
    document.body,
  );
}

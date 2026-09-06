import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { Modal } from "./Modal";
import FolderPicker from "./FolderPicker";
import { btnGhost, btnPrimary } from "./ui";
import { markdownPath, studioPath } from "@/lib/routes";

/** Open a skill folder or a standalone Markdown file on the active server. */
export default function OpenSkillDialog({ onClose }: { onClose: () => void }) {
  const navigate = useNavigate();
  const [path, setPath] = useState("");
  const [browsing, setBrowsing] = useState(false);
  const open = (target: string, markdown: boolean) => {
    onClose();
    navigate(markdown ? markdownPath(target) : studioPath(target));
  };

  if (browsing) {
    return (
      <FolderPicker
        onClose={onClose}
        onSelect={(target) => open(target, false)}
        onSelectFile={(target) => open(target, true)}
      />
    );
  }

  return (
    <Modal title="Open a skill or Markdown file" onClose={onClose}>
      <form
        className="space-y-4 px-5 py-4"
        onSubmit={(e) => {
          e.preventDefault();
          const target = path.trim();
          if (target) open(target, /\.(md|markdown|mdx)$/i.test(target));
        }}
      >
        <p className="text-xs leading-relaxed text-muted">
          A skill is a folder containing a{" "}
          <code className="rounded bg-panel px-1 py-0.5 font-mono text-[0.85em]">SKILL.md</code>. Paste its path
          (or a loose Markdown file’s), or browse for it.
        </p>
        <input
          aria-label="Skill folder or Markdown file path"
          value={path}
          onChange={(e) => setPath(e.target.value)}
          placeholder="/absolute/path/to/skill-folder"
          spellCheck={false}
          autoFocus
          className="w-full rounded-md border border-border bg-surface px-2.5 py-1.5 font-mono text-sm text-fg outline-none focus:border-accent"
        />
        <div className="flex justify-end gap-2 pt-1">
          <button type="button" onClick={() => setBrowsing(true)} className={btnGhost}>
            Browse…
          </button>
          <button type="submit" disabled={!path.trim()} className={btnPrimary}>
            Open
          </button>
        </div>
      </form>
    </Modal>
  );
}

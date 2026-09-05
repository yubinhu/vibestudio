import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { Modal } from "./Modal";
import FolderPicker from "./FolderPicker";
import RecentItems from "./RecentItems";
import { markdownPath, studioPath } from "@/lib/routes";

export default function RecentDialog({ onClose }: { onClose: () => void }) {
  const [browsing, setBrowsing] = useState(false);
  const navigate = useNavigate();

  if (browsing) {
    return (
      <FolderPicker
        onClose={onClose}
        onSelect={(path) => { navigate(studioPath(path)); onClose(); }}
        onSelectFile={(path) => { navigate(markdownPath(path)); onClose(); }}
      />
    );
  }

  return (
    <Modal title="Recent" onClose={onClose} widthClass="max-w-2xl">
      <div className="max-h-[70dvh] overflow-y-auto">
        <RecentItems onOpen={() => setBrowsing(true)} />
      </div>
    </Modal>
  );
}

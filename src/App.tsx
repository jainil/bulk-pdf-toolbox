import { useState } from "react";
import { Icon } from "./components/Icon";
import { OcrTool } from "./tools/ocr/OcrTool";

const TOOLS = [
  { id: "ocr", name: "Bulk OCR", icon: "ocr", ready: true },
  { id: "merge", name: "Merge", icon: "merge", ready: false },
  { id: "split", name: "Split", icon: "split", ready: false },
  { id: "compress", name: "Compress", icon: "compress", ready: false },
  { id: "extract", name: "Extract text", icon: "text", ready: false },
] as const;

export default function App() {
  const [tool, setTool] = useState<string>("ocr");
  return (
    <div className="app">
      <nav className="sidebar">
        <div className="brand">
          <div className="brand-mark">
            <Icon name="file" size={16} />
          </div>
          <span>PDF Toolbox</span>
        </div>
        <div className="nav-section">Tools</div>
        {TOOLS.map((t) => (
          <button
            key={t.id}
            className={`nav-item ${tool === t.id ? "active" : ""}`}
            disabled={!t.ready}
            onClick={() => setTool(t.id)}
            title={t.ready ? t.name : `${t.name} — coming soon`}
          >
            <Icon name={t.icon} />
            <span>{t.name}</span>
            {!t.ready && <span className="soon">Soon</span>}
          </button>
        ))}
      </nav>
      <main className="main">{tool === "ocr" && <OcrTool />}</main>
    </div>
  );
}

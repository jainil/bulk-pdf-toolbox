import { useState, type ReactNode } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import type { Settings } from "../../lib/types";
import { TRASH, tildify } from "../../lib/format";
import { Icon } from "../../components/Icon";
import { InfoTip } from "../../components/InfoTip";

const LANG_NAMES: Record<string, string> = {
  eng: "English", deu: "German", fra: "French", spa: "Spanish", ita: "Italian", por: "Portuguese",
  nld: "Dutch", hin: "Hindi", guj: "Gujarati", mar: "Marathi", ben: "Bengali", tam: "Tamil",
  chi_sim: "Chinese (Simplified)", chi_tra: "Chinese (Traditional)", jpn: "Japanese", kor: "Korean",
  rus: "Russian", ara: "Arabic", heb: "Hebrew", pol: "Polish", tur: "Turkish", ukr: "Ukrainian",
};

const ADVANCED_KEY = "bpt.settings.advanced";

function loadAdvanced(): boolean {
  try {
    return localStorage.getItem(ADVANCED_KEY) === "1";
  } catch {
    return false;
  }
}

interface Props {
  settings: Settings;
  languages: string[];
  locked: boolean;
  onChange: (s: Settings) => void;
  onClose: () => void;
}

function Section({ title, info, children }: { title: string; info?: ReactNode; children: ReactNode }) {
  return (
    <section>
      <div className="section-head">
        <h4>{title}</h4>
        {info && <InfoTip label={`About ${title}`}>{info}</InfoTip>}
      </div>
      {children}
    </section>
  );
}

function Field({ label, info, children }: { label: string; info?: ReactNode; children: ReactNode }) {
  return (
    <label className="row-field">
      <span className="field-label">
        {label}
        {info && <InfoTip label={`About ${label}`}>{info}</InfoTip>}
      </span>
      {children}
    </label>
  );
}

export function SettingsPanel({ settings: s, languages, locked, onChange, onClose }: Props) {
  const [advanced, setAdvanced] = useState(loadAdvanced);
  const set = <K extends keyof Settings>(k: K, v: Settings[K]) => onChange({ ...s, [k]: v });
  const langs = Array.from(new Set([...languages, ...s.languages]));

  const toggleAdvanced = () => {
    const next = !advanced;
    setAdvanced(next);
    try {
      localStorage.setItem(ADVANCED_KEY, next ? "1" : "0");
    } catch {
      /* preference only */
    }
  };

  const toggleLang = (l: string) => {
    const has = s.languages.includes(l);
    const next = has ? s.languages.filter((x) => x !== l) : [...s.languages, l];
    if (next.length) set("languages", next);
  };

  const pickOut = async () => {
    const d = await open({ directory: true });
    if (typeof d === "string") onChange({ ...s, outputDir: d, outputMode: "folder" });
  };

  return (
    <aside className="island settings">
      <div className="island-head">
        <h3>Settings</h3>
        <button className="icon-btn" onClick={onClose} title="Hide settings">
          <Icon name="x" />
        </button>
      </div>

      <div className="settings-body">
        {locked && (
          <div className="note">
            <Icon name="clock" size={15} />
            Changes apply to files that start next.
          </div>
        )}

        <Section
          title="Languages"
          info="Pick every language that appears in your documents. Fewer languages is faster and more accurate."
        >
          <div className="chips">
            {langs.map((l) => (
              <button
                key={l}
                className={`chip ${s.languages.includes(l) ? "on" : ""} ${!languages.includes(l) ? "missing" : ""}`}
                onClick={() => toggleLang(l)}
                title={!languages.includes(l) ? "Language data not installed" : undefined}
              >
                {s.languages.includes(l) && <Icon name="check" size={13} />}
                {LANG_NAMES[l] ?? l}
              </button>
            ))}
          </div>
        </Section>

        <Section
          title="Pages with text"
          info={
            <dl>
              <dt>Skip those pages</dt>
              <dd>Only scanned pages are OCR'd. Pages that already have text stay untouched.</dd>
              <dt>Redo existing OCR</dt>
              <dd>Replaces an old, poor OCR layer while keeping real (typed) text.</dd>
              <dt>Force OCR</dt>
              <dd>Turns every page into an image and OCRs it. Most thorough, but loses vector sharpness.</dd>
              <dt>Skip whole file</dt>
              <dd>Leaves any file that already contains text alone.</dd>
            </dl>
          }
        >
          <select value={s.mode} onChange={(e) => set("mode", e.target.value as Settings["mode"])}>
            <option value="skip">Skip those pages (recommended)</option>
            <option value="redo">Redo existing OCR</option>
            <option value="force">Force OCR on every page</option>
            <option value="default">Skip whole file</option>
          </select>
        </Section>

        <Section title="Save to">
          <div className="radio-list">
            <label>
              <input type="radio" checked={s.outputMode === "suffix"} onChange={() => set("outputMode", "suffix")} />
              Next to original
            </label>
            {s.outputMode === "suffix" && (
              <div className="indent inline-field">
                <span className="muted">Name ends with</span>
                <input value={s.suffix} onChange={(e) => set("suffix", e.target.value)} placeholder="_ocr" />
              </div>
            )}
            <label>
              <input type="radio" checked={s.outputMode === "folder"} onChange={() => (s.outputDir ? set("outputMode", "folder") : pickOut())} />
              Another folder
              <InfoTip>Subfolders of added folders are recreated inside the chosen folder.</InfoTip>
            </label>
            {s.outputMode === "folder" && (
              <button className="btn small indent path-btn" onClick={pickOut} title={s.outputDir ?? ""}>
                <Icon name="folder" size={14} />
                <span>{s.outputDir ? tildify(s.outputDir) : "Choose folder…"}</span>
              </button>
            )}
            <label>
              <input type="radio" checked={s.outputMode === "replace"} onChange={() => set("outputMode", "replace")} />
              Replace original
              <InfoTip>Each original goes to the {TRASH} only after its OCR succeeds, so you can still restore it.</InfoTip>
            </label>
          </div>
          {s.outputMode !== "replace" && (
            <Field label="If file exists">
              <select value={s.conflict} onChange={(e) => set("conflict", e.target.value as Settings["conflict"])}>
                <option value="rename">Keep both</option>
                <option value="overwrite">Overwrite</option>
                <option value="skip">Skip file</option>
              </select>
            </Field>
          )}
        </Section>

        <Section title="Clean up scans">
          <label className="check">
            <input type="checkbox" checked={s.rotatePages} onChange={(e) => set("rotatePages", e.target.checked)} />
            Fix page rotation
            <InfoTip>Turns sideways or upside-down pages the right way up.</InfoTip>
          </label>
          <label className={`check ${s.mode === "redo" ? "disabled" : ""}`}>
            <input type="checkbox" disabled={s.mode === "redo"} checked={s.deskew && s.mode !== "redo"} onChange={(e) => set("deskew", e.target.checked)} />
            Straighten tilted pages
            <InfoTip>
              Rotates slightly crooked scans so lines are level.
              {s.mode === "redo" && " Not available with “Redo existing OCR”."}
            </InfoTip>
          </label>
        </Section>

        <button className={`advanced-toggle ${advanced ? "open" : ""}`} onClick={toggleAdvanced} aria-expanded={advanced}>
          <Icon name="chevron" size={15} />
          Advanced options
        </button>

        {advanced && (
          <>
            <Section title="File format">
              <Field
                label="Format"
                info="Auto makes archival PDF/A when the result validates, otherwise a regular PDF. PDF/A marks files that can't be converted as failed. Plain PDF changes the file the least."
              >
                <select value={s.outputType} onChange={(e) => set("outputType", e.target.value as Settings["outputType"])}>
                  <option value="auto">Auto</option>
                  <option value="pdfa">PDF/A</option>
                  <option value="pdf">Plain PDF</option>
                </select>
              </Field>
              <Field
                label="Optimize"
                info="Lossless shrinks files without touching image quality. Lossy and Aggressive recompress images for much smaller files at some quality cost."
              >
                <select value={s.optimize} onChange={(e) => set("optimize", Number(e.target.value))}>
                  <option value={0}>Off</option>
                  <option value={1}>Lossless</option>
                  <option value={2}>Lossy</option>
                  <option value={3}>Aggressive</option>
                </select>
              </Field>
              <label className="check">
                <input type="checkbox" checked={s.sidecar} onChange={(e) => set("sidecar", e.target.checked)} />
                Also save text as .txt
              </label>
              <label className="check">
                <input type="checkbox" checked={s.invalidateSignatures} onChange={(e) => set("invalidateSignatures", e.target.checked)} />
                Process signed PDFs
                <InfoTip>
                  Digitally signed PDFs are skipped by default, because adding text changes the file and breaks the
                  signature. Turn this on to OCR them anyway — the copy will no longer verify as signed. Your original
                  is kept unless you chose “Replace original”.
                </InfoTip>
              </label>
            </Section>

            <Section title="Performance">
              <Field label="Files at once" info="How many PDFs are processed in parallel.">
                <input type="number" min={1} max={64} value={s.parallelFiles} onChange={(e) => set("parallelFiles", Math.max(1, Number(e.target.value) || 1))} />
              </Field>
              <Field label="Threads per file" info="CPU threads each file may use. Files × threads should roughly match your CPU core count.">
                <input type="number" min={1} max={64} value={s.jobsPerFile} onChange={(e) => set("jobsPerFile", Math.max(1, Number(e.target.value) || 1))} />
              </Field>
              <Field label="Auto-retry" info="How many times a file is retried after a temporary failure, such as a crash or a stall.">
                <input type="number" min={0} max={10} value={s.maxRetries} onChange={(e) => set("maxRetries", Math.max(0, Number(e.target.value) || 0))} />
              </Field>
              <Field label="Stall timeout (min)" info="A file is stopped if it makes no progress for this many minutes. Paused time doesn't count. 0 turns this off.">
                <input type="number" min={0} max={600} value={s.stallTimeoutMin} onChange={(e) => set("stallTimeoutMin", Math.max(0, Number(e.target.value) || 0))} />
              </Field>
            </Section>
          </>
        )}
      </div>
    </aside>
  );
}

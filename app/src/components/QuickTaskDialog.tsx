import React, {
  useState,
  useEffect,
  useRef,
  useCallback,
  useLayoutEffect,
} from "react";
import { createPortal } from "react-dom";
import { Modal, Btn, Select } from "./atoms";
import { I } from "./icons";
import { TaskConfigChip } from "./TaskConfigChip";
import { useProject, LANG_DOT } from "./ProjectPicker";
import { useTaskRuntime } from "../lib/taskRuntime";
import { isTauri } from "../lib/tauri";
import * as ipc from "../lib/ipc";
import type { ProjectFileEntry } from "../lib/ipc";
import type { ImageAttachment } from "../lib/types";
import { FileMentionPicker } from "./FileMentionPicker";
import { detectMention, type MentionSpan } from "../lib/mention";

const QUICK_TASK_PREFS_KEY = "hyvemind:quick-task-prefs";

interface QuickTaskPrefs {
  model?: string;
  hivemind?: string | null;
  thinking?: string;
}

function loadPrefs(): QuickTaskPrefs {
  try {
    const raw = localStorage.getItem(QUICK_TASK_PREFS_KEY);
    return raw ? JSON.parse(raw) : {};
  } catch {
    return {};
  }
}

function savePrefs(p: QuickTaskPrefs) {
  try {
    localStorage.setItem(QUICK_TASK_PREFS_KEY, JSON.stringify(p));
  } catch {}
}

export function QuickTaskDialog({
  open,
  onClose,
  prefill,
}: {
  open: boolean;
  onClose: () => void;
  prefill?: string;
}) {
  const { project, setProject, projects } = useProject();
  const { defaultModel, defaultHivemind, hivemindOptions, createTask } = useTaskRuntime();

  const [model, setModel] = useState<string>(
    () => loadPrefs().model || defaultModel || "",
  );
  const [hivemind, setHivemind] = useState<string | null>(
    () => {
      const prefs = loadPrefs();
      // If user explicitly saved a preference (even null for "None"), use it.
      // Otherwise fall back to the default hivemind.
      if ("hivemind" in prefs) return prefs.hivemind ?? null;
      return defaultHivemind || null;
    },
  );
  const [thinking, setThinking] = useState<string>(
    () => loadPrefs().thinking || "high",
  );
  const [prompt, setPrompt] = useState("");
  const promptRef = useRef(prompt);
  promptRef.current = prompt;
  const taRef = useRef<HTMLTextAreaElement>(null);

  // ── Pending image attachments ──────────────────────────
  const [pendingImages, setPendingImages] = useState<ImageAttachment[]>([]);
  const pendingImagesRef = useRef<ImageAttachment[]>([]);
  pendingImagesRef.current = pendingImages;

  // ── Attached files (from @-mention picker) ──────────────
  const [attachedFiles, setAttachedFiles] = useState<string[]>([]);

  // ── Mention picker state ───────────────────────────────
  const [mention, setMention] = useState<MentionSpan | null>(null);
  const mentionRef = useRef<MentionSpan | null>(null);
  mentionRef.current = mention;
  const [mentionSelected, setMentionSelected] = useState(0);
  const mentionSelectedRef = useRef(0);
  mentionSelectedRef.current = mentionSelected;
  const mentionItemsRef = useRef<ProjectFileEntry[]>([]);
  const ignoredMentionStartRef = useRef<number | null>(null);
  const caretAfterPickRef = useRef<number | null>(null);
  const cursorPosRef = useRef<number>(0);

  // ── Picker position (portal placement) ────────────────
  const [pickerPos, setPickerPos] = useState<{
    left: number;
    bottom: number;
    width: number;
  } | null>(null);
  const textareaWrapRef = useRef<HTMLDivElement>(null);

  // ── Open/close transition + submit lifecycle refs ─────
  const prevOpenRef = useRef(false);
  const submittedRef = useRef(false);
  // dialogOpenRef mirrors `open` so async callbacks (FileReader, etc.)
  // can detect "dialog closed since I started" without re-subscribing.
  // The component itself is NOT unmounted on close; only the inner Modal
  // returns null, so isMountedRef would be the wrong primitive.
  const dialogOpenRef = useRef(open);
  dialogOpenRef.current = open;
  const projectRef = useRef(project);
  projectRef.current = project;

  useEffect(() => {
    if (!loadPrefs().model && defaultModel && !model) setModel(defaultModel);
  }, [defaultModel]);

  useEffect(() => {
    if (!open) return;
    const prefs = loadPrefs();
    // Only apply the default if the user has never saved a hivemind preference at all
    if (!("hivemind" in prefs) && defaultHivemind && !hivemind) {
      setHivemind(defaultHivemind);
    }
  }, [defaultHivemind, open]);

  // ── Open/close transition effect (combined prefill + cleanup) ──
  useEffect(() => {
    const wasOpen = prevOpenRef.current;
    prevOpenRef.current = open;

    if (!wasOpen && open) {
      // ── Opening: reset for fresh session ──
      submittedRef.current = false;
      setPendingImages([]);
      pendingImagesRef.current = [];
      setAttachedFiles([]);
      setMention(null);
      setMentionSelected(0);
      ignoredMentionStartRef.current = null;
      mentionItemsRef.current = [];
      caretAfterPickRef.current = null;

      // Apply prefill (if any)
      if (prefill) setPrompt(prefill);
      setTimeout(() => {
        const ta = taRef.current;
        if (!ta) return;
        ta.focus();
        if (prefill) {
          const len = ta.value.length;
          ta.setSelectionRange(len, len);
        }
      }, 0);
    }

    if (wasOpen && !open) {
      // ── Closing: clean up ──
      // Revoke object URLs only if the images were NOT submitted
      // (submitted images are now owned by the new task's render path).
      if (!submittedRef.current) {
        pendingImagesRef.current.forEach((img) =>
          URL.revokeObjectURL(img.previewUrl),
        );
      }
      // Clear state unconditionally — component may stay mounted when
      // open={false}, so stale state would leak into the next session.
      pendingImagesRef.current = [];
      setPendingImages([]);
      setAttachedFiles([]);
      setMention(null);
      setMentionSelected(0);
      ignoredMentionStartRef.current = null;
      mentionItemsRef.current = [];
      caretAfterPickRef.current = null;
      submittedRef.current = false;
    }
  }, [open]);

  // ── Unmount cleanup (fallback) ───────────────────────
  useEffect(() => {
    return () => {
      if (!submittedRef.current) {
        pendingImagesRef.current.forEach((img) =>
          URL.revokeObjectURL(img.previewUrl),
        );
      }
    };
  }, []);

  // ── Clear attached files + mention state on project change ──
  useEffect(() => {
    setAttachedFiles([]);
    setMention(null);
    setMentionSelected(0);
    ignoredMentionStartRef.current = null;
    caretAfterPickRef.current = null;
  }, [project?.cwd]);

  // ── Picker position: compute once when mention opens ────────
  // The Modal wraps content in `overflow-y-auto`, which would clip an
  // absolutely-positioned FileMentionPicker. We portal it to document.body
  // and position it relative to the textarea wrapper's viewport rect.
  useLayoutEffect(() => {
    if (!mention) {
      if (pickerPos !== null) setPickerPos(null);
      return;
    }
    const wrap = textareaWrapRef.current;
    if (!wrap) return;
    const rect = wrap.getBoundingClientRect();
    setPickerPos({
      left: rect.left,
      // `bottom` here means the y-coord that becomes the picker wrapper's
      // `top`: the BOTTOM edge of the textarea, so the picker drops just
      // below it (dropUp=false → picker uses `top-full mt-1` inside).
      bottom: rect.bottom,
      width: rect.width,
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mention?.startIdx]);

  // ── Close picker on window scroll/resize ───────────────
  // Simpler and more predictable than trying to follow the textarea
  // through the modal's overflow-y-auto scroll.
  useEffect(() => {
    if (!mention) return;
    const onScroll = () => setMention(null);
    const onResize = () => setMention(null);
    window.addEventListener("scroll", onScroll, {
      capture: true,
      passive: true,
    });
    window.addEventListener("resize", onResize);
    return () => {
      window.removeEventListener(
        "scroll",
        onScroll,
        { capture: true } as any,
      );
      window.removeEventListener("resize", onResize);
    };
  }, [mention]);

  // ── Paste handler for images ──────────────────────────
  const handlePaste = useCallback((e: React.ClipboardEvent) => {
    const items = e.clipboardData?.items;
    if (!items) return;
    const imageItems: DataTransferItem[] = [];
    for (let i = 0; i < items.length; i++) {
      if (items[i].type.startsWith("image/")) imageItems.push(items[i]);
    }
    if (imageItems.length === 0) return;
    e.preventDefault();
    for (const item of imageItems) {
      const file = item.getAsFile();
      if (!file) continue;

      // Reject files larger than 5 MB
      if (file.size > 5 * 1024 * 1024) {
        console.warn("Skipping pasted image over 5 MB:", file.size);
        continue;
      }

      // Capture preview URL + mediaType BEFORE reading so they live in the
      // closure regardless of when onload fires.
      const mediaType = file.type || "image/png";
      const previewUrl = URL.createObjectURL(file);
      const reader = new FileReader();
      reader.onload = () => {
        // Dialog-closed guard. The component does not unmount on close, so
        // we check dialogOpenRef rather than an isMountedRef. If the user
        // closed the dialog while the read was in flight, revoke the
        // previewUrl we just created (no consumer) and bail.
        if (!dialogOpenRef.current) {
          URL.revokeObjectURL(previewUrl);
          return;
        }
        const dataUrl = reader.result as string;
        const commaIdx = dataUrl.indexOf(",");
        const base64 = dataUrl.slice(commaIdx + 1);
        setPendingImages((prev) => {
          if (prev.length >= 10) {
            URL.revokeObjectURL(previewUrl);
            return prev; // max 10 pending images
          }
          return [
            ...prev,
            { id: crypto.randomUUID(), mediaType, data: base64, previewUrl },
          ];
        });
      };
      reader.onerror = () => {
        URL.revokeObjectURL(previewUrl);
        console.warn("Failed to read pasted image:", reader.error);
      };
      reader.readAsDataURL(file);
    }
  }, []);

  const removeImage = useCallback((id: string) => {
    setPendingImages((prev) => {
      const img = prev.find((i) => i.id === id);
      if (img) URL.revokeObjectURL(img.previewUrl);
      return prev.filter((i) => i.id !== id);
    });
  }, []);

  const removeAttachedFile = useCallback(
    (p: string) => setAttachedFiles((prev) => prev.filter((x) => x !== p)),
    [],
  );

  // ── Mention detection ──────────────────────────────────
  // NOTE: empty deps — reads state via refs so the callback identity is
  // stable across renders.
  // TODO: IME composition — mention detection runs on every onChange and
  // could open the picker during intermediate IME states. Pre-existing in
  // the Tasks composer; not addressed here.
  const updateMention = useCallback((val: string, cursor: number) => {
    if (!projectRef.current?.cwd) {
      if (mentionRef.current !== null) setMention(null);
      return;
    }
    const cur = mentionRef.current;
    const next = detectMention(val, cursor);
    if (
      ignoredMentionStartRef.current !== null &&
      (next === null || next.startIdx !== ignoredMentionStartRef.current)
    ) {
      ignoredMentionStartRef.current = null;
    }
    if (next !== null && ignoredMentionStartRef.current === next.startIdx) {
      if (cur !== null) setMention(null);
      return;
    }
    if (next === null && cur === null) return;
    if (
      next !== null &&
      cur !== null &&
      next.startIdx === cur.startIdx &&
      next.tokenEnd === cur.tokenEnd
    ) {
      return;
    }
    setMention(next);
  }, []);

  const handleChange = useCallback(
    (e: React.ChangeEvent<HTMLTextAreaElement>) => {
      const v = e.target.value;
      promptRef.current = v;
      setPrompt(v);
      const cursor = e.target.selectionStart ?? v.length;
      cursorPosRef.current = cursor;
      updateMention(v, cursor);
    },
    [updateMention],
  );

  const handleSelect = useCallback(
    (e: React.SyntheticEvent<HTMLTextAreaElement>) => {
      const ta = e.currentTarget;
      cursorPosRef.current = ta.selectionStart ?? 0;
      updateMention(ta.value, cursorPosRef.current);
    },
    [updateMention],
  );

  const handleKeyUp = useCallback(
    (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
      const ta = e.currentTarget;
      cursorPosRef.current = ta.selectionStart ?? 0;
      updateMention(ta.value, cursorPosRef.current);
    },
    [updateMention],
  );

  const handleFocus = useCallback(
    (e: React.FocusEvent<HTMLTextAreaElement>) => {
      const ta = e.currentTarget;
      cursorPosRef.current = ta.selectionStart ?? 0;
      updateMention(ta.value, cursorPosRef.current);
    },
    [updateMention],
  );

  const handleBlur = useCallback(() => {
    setMention(null);
  }, []);

  const applyPick = useCallback((filePath: string) => {
    const m = mentionRef.current;
    if (!m) return;
    const v = promptRef.current;
    const before = v.slice(0, m.startIdx);
    const after = v.slice(m.tokenEnd);
    const needsSpace =
      before.length > 0 &&
      !/\s$/.test(before) &&
      (after.length === 0 || !/^\s/.test(after));
    const newValue = before + (needsSpace ? " " : "") + after;
    const caretPos = m.startIdx + (needsSpace ? 1 : 0);
    promptRef.current = newValue;
    setPrompt(newValue);
    setMention(null);
    setMentionSelected(0);
    mentionItemsRef.current = [];
    ignoredMentionStartRef.current = null;
    caretAfterPickRef.current = caretPos;
    setAttachedFiles((prev) => {
      if (prev.includes(filePath)) return prev;
      if (prev.length >= 20) {
        console.warn("File attachment limit (20) reached");
        return prev;
      }
      return [...prev, filePath];
    });
    requestAnimationFrame(() => {
      const ta = taRef.current;
      if (!ta) return;
      ta.focus();
      if (caretAfterPickRef.current !== null) {
        ta.setSelectionRange(
          caretAfterPickRef.current,
          caretAfterPickRef.current,
        );
        caretAfterPickRef.current = null;
      }
    });
  }, []);

  const handleItemsChange = useCallback((items: ProjectFileEntry[]) => {
    mentionItemsRef.current = items;
    setMentionSelected(0);
  }, []);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
      const m = mentionRef.current;
      if (!m) return;
      const items = mentionItemsRef.current;
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setMentionSelected((i) =>
          Math.min(i + 1, Math.max(0, items.length - 1)),
        );
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setMentionSelected((i) => Math.max(0, i - 1));
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        // We intentionally do NOT rely on stopPropagation() to block the
        // window-level Escape → onClose handler — React's synthetic-event
        // stopPropagation does not stop native window listeners reliably.
        // The window listener itself checks mentionRef.current and bails
        // when the picker is open (see open-effect below).
        ignoredMentionStartRef.current = m.startIdx;
        setMention(null);
        return;
      }
      if (
        (e.key === "Enter" && !e.shiftKey && !e.metaKey && !e.ctrlKey) ||
        e.key === "Tab"
      ) {
        const pickedIndex = mentionSelectedRef.current;
        if (
          items.length > 0 &&
          pickedIndex >= 0 &&
          pickedIndex < items.length
        ) {
          e.preventDefault();
          applyPick(items[pickedIndex].path);
          return;
        }
        // No items.
        if (e.key === "Enter") {
          // Close picker, do NOT insert a newline, do NOT submit. QuickTask's
          // submit shortcut is Cmd-Enter; plain Enter must do nothing
          // dangerous when the picker was open and is dismissed by the same
          // keystroke. Mark this @-position as ignored so the immediately
          // following onKeyUp -> updateMention doesn't re-open the picker.
          e.preventDefault();
          ignoredMentionStartRef.current = m.startIdx;
          setMention(null);
          return;
        }
        // Tab with no items: close picker and allow tab-out (no preventDefault).
        ignoredMentionStartRef.current = m.startIdx;
        setMention(null);
        return;
      }
      // For Cmd/Ctrl+Enter and any other key, fall through to window listener.
    },
    [applyPick],
  );

  const query = mention
    ? prompt.slice(
        mention.startIdx + 1,
        Math.min(
          cursorPosRef.current ?? mention.tokenEnd,
          mention.tokenEnd,
        ),
      )
    : "";

  const submit = () => {
    // Guard against double-submit from rapid Cmd+Enter or key repeat.
    if (submittedRef.current) return;

    const txt = prompt.trim();
    if (!txt && pendingImages.length === 0 && attachedFiles.length === 0) return;

    // Sanitize file paths defensively — newlines would corrupt the block
    // format. Paths come from ipc.listProjectFiles (project-controlled),
    // so this is defense-in-depth, not protection against user input.
    const safePaths = attachedFiles.map((p) => p.replace(/[\r\n]+/g, " "));
    const filesBlock = safePaths.length
      ? `\n\n[Attached files]\n${safePaths.map((p) => `- ${p}`).join("\n")}`
      : "";
    let finalText = (txt + filesBlock).trim();

    // Backend safety net: if we have images but no text content at all,
    // send a minimal placeholder so the Pi subprocess and any downstream
    // consumers that interpret empty `message` as "missing" still see a
    // body. submitMessageImpl already allows empty text with images, but
    // the IPC contract for empty `message: ""` with images is not
    // exhaustively verified across all providers.
    if (!finalText && pendingImages.length > 0) {
      finalText = "(image attachment)";
    }

    // Compute a clean display title from the raw user text (without the
    // [Attached files] block).
    const title = txt
      ? txt.slice(0, 80)
      : pendingImages.length > 0
        ? "Image task"
        : attachedFiles.length > 0
          ? "File task"
          : "New Task";

    const resolvedModel = model || defaultModel || "";
    const sentImages = pendingImages.length > 0 ? [...pendingImages] : undefined;

    savePrefs({ model: resolvedModel, hivemind, thinking });
    if (isTauri()) {
      ipc.setDefaultModel(resolvedModel).catch(() => {});
    }

    // Mark as submitted BEFORE clearing state or closing — prevents the
    // close/unmount cleanup from revoking object URLs that are now owned by
    // the new task's message render path.
    submittedRef.current = true;
    // Clear the ref synchronously so the close cleanup (which reads
    // pendingImagesRef.current) sees an empty array even if React hasn't
    // committed the state update yet.
    pendingImagesRef.current = [];

    createTask({
      prompt: finalText,
      title,
      model: resolvedModel,
      hivemind,
      thinking,
      projectPath: project?.cwd ?? null,
      setActive: false,
      autoMode: true,
      images: sentImages,
    });

    setPrompt("");
    setPendingImages([]);
    setAttachedFiles([]);
    onClose();
  };

  // Ref always pointing to the latest `submit` so window listener doesn't
  // need every state dep. Plain assignment (not a hook) so each render's
  // closure (which captures fresh state) is used.
  const submitRef = useRef(submit);
  submitRef.current = submit;

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        // Defensive: React's synthetic-event stopPropagation does NOT
        // reliably block native window keydown listeners. So when the
        // mention picker is open, we bail here — the textarea-level
        // handler will close the picker on this same keystroke. A second
        // Escape (picker now closed) reaches here and closes the dialog.
        if (mentionRef.current) return;
        onClose();
        return;
      }
      if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
        e.preventDefault();
        submitRef.current();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  const projectOptions = projects.map((p) => ({
    value: p.id,
    label: `${p.org}/${p.name}`,
  }));

  return (
    <Modal open={open} onClose={onClose} title="Quick Task" wide>
      <div className="flex flex-col gap-4">
        <div className="flex flex-col gap-1.5">
          <label className="text-[11px] uppercase tracking-wide text-dim">
            Project
          </label>
          <div className="flex items-center gap-2">
            {project && (
              <span
                className={`w-2.5 h-2.5 rounded-full shrink-0 ${LANG_DOT[project.lang] || "bg-muted"}`}
              />
            )}
            <Select
              wrapClass="flex-1"
              options={projectOptions}
              value={project?.id ?? ""}
              onChange={(e) => {
                const found = projects.find((p) => p.id === e.target.value);
                if (found) setProject(found);
              }}
            />
          </div>
        </div>

        <div>
          <TaskConfigChip
            model={model}
            onModelChange={setModel}
            hivemind={hivemind}
            onHivemindChange={setHivemind}
            onThinkingChange={setThinking}
            hivemindOptions={hivemindOptions}
          />
        </div>

        {pendingImages.length > 0 && (
          <div className="flex gap-2 flex-wrap">
            {pendingImages.map((img) => (
              <div key={img.id} className="relative group">
                <img
                  src={img.previewUrl}
                  alt="Pending"
                  className="w-16 h-16 object-cover rounded-lg border border-line"
                />
                <button
                  onClick={() => removeImage(img.id)}
                  className="absolute -top-1.5 -right-1.5 w-5 h-5 rounded-full bg-ink-900 border border-line flex items-center justify-center text-dim hover:text-white hover:bg-red-500/80 transition-colors opacity-0 group-hover:opacity-100"
                >
                  {I.x({ size: 10 })}
                </button>
              </div>
            ))}
          </div>
        )}

        {attachedFiles.length > 0 && (
          <div
            data-testid="attached-files"
            className="flex gap-1.5 flex-wrap"
          >
            {attachedFiles.map((p) => (
              <span
                key={p}
                className="inline-flex items-center gap-1.5 h-6 px-2 rounded-md bg-ink-800 border border-line text-[11px] text-white/85"
              >
                {I.doc({ size: 11, className: "text-honey-400/80" })}
                <span
                  className="font-mono truncate max-w-[260px]"
                  title={p}
                >
                  {p}
                </span>
                <button
                  aria-label={`Remove ${p}`}
                  onClick={() => removeAttachedFile(p)}
                  className="text-dim hover:text-red-400"
                >
                  {I.x({ size: 10 })}
                </button>
              </span>
            ))}
          </div>
        )}

        <div
          ref={textareaWrapRef}
          className="relative bg-ink-800 border border-line focus-within:border-honey-500/40 rounded-md transition-colors"
        >
          <textarea
            ref={taRef}
            value={prompt}
            onChange={handleChange}
            onPaste={handlePaste}
            onKeyDown={handleKeyDown}
            onKeyUp={handleKeyUp}
            onSelect={handleSelect}
            onFocus={handleFocus}
            onBlur={handleBlur}
            placeholder="What do you want to build?"
            rows={6}
            className="w-full bg-transparent resize-none px-3 py-2 text-[13px] focus:outline-none focus-visible:ring-2 focus-visible:ring-honey-500 focus-visible:ring-offset-2 focus-visible:ring-offset-ink-950 placeholder:text-dim text-white/90 min-h-[120px]"
          />
        </div>

        <div className="flex items-center justify-end text-[11px] text-dim">
          <div className="flex items-center gap-2">
            <Btn kind="ghost" onClick={onClose}>
              Cancel
            </Btn>
            <Btn
              kind="primary"
              onClick={submit}
              disabled={
                !prompt.trim() &&
                pendingImages.length === 0 &&
                attachedFiles.length === 0
              }
              icon={I.rocket({ size: 13 })}
            >
              Create
            </Btn>
          </div>
        </div>
      </div>
      {mention && pickerPos &&
        createPortal(
          <div
            // The portal wrapper is `position: fixed` so the picker is not
            // clipped by the modal's `overflow-y-auto`. z-[60] explicitly
            // outranks the modal overlay (z-50).
            className="fixed z-[60]"
            style={{
              left: pickerPos.left,
              top: pickerPos.bottom,
              width: pickerPos.width,
            }}
          >
            <FileMentionPicker
              open={!!mention}
              query={query}
              projectPath={project?.cwd ?? null}
              selectedIndex={mentionSelected}
              onSetSelection={setMentionSelected}
              onPick={applyPick}
              onItemsChange={handleItemsChange}
              dropUp={false}
            />
          </div>,
          document.body,
        )}
    </Modal>
  );
}

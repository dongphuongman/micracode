"use client";

import { useCallback, useEffect, useRef, useState } from "react";

import { streamEvents } from "@/lib/api/commands";
import { listProjects } from "@/lib/api/projects";
import {
  captureCheckpoint,
  getCheckpointDiff,
  listCheckpoints,
  revertCheckpoint,
  type Checkpoint,
} from "@/lib/api/vcs";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

/**
 * Per-turn checkpoints for a project's workspace (PRD FR6). The Rust core
 * snapshots the working tree into Git after every turn; this panel lists those
 * snapshots, renders the diff a snapshot introduced, and can revert the
 * workspace back to one. The SSE event stream is used as a "something changed"
 * signal — each tick refetches the list, so an auto-captured checkpoint shows up
 * without a manual refresh.
 *
 * With no `projectId` it falls back to the first project, so it renders live on
 * the home page without a project router.
 */
export function CheckpointsPanel({
  projectId: projectIdProp,
  className,
}: {
  projectId?: string;
  className?: string;
}) {
  const [projectId, setProjectId] = useState<string | null>(projectIdProp ?? null);
  const [checkpoints, setCheckpoints] = useState<Checkpoint[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [diff, setDiff] = useState<string>("");
  const [busy, setBusy] = useState(false);

  const projectRef = useRef<string | null>(projectId);
  projectRef.current = projectId;
  const selectedRef = useRef<string | null>(null);
  selectedRef.current = selectedId;

  // Resolve the project once (prop, else first project), then load checkpoints.
  const refresh = useCallback(async () => {
    try {
      let id = projectRef.current;
      if (!id) {
        id = (await listProjects())[0]?.id ?? null;
        if (id) setProjectId(id);
      }
      if (!id) return;
      const list = await listCheckpoints(id);
      setCheckpoints(list);
      const sel = selectedRef.current ?? list[0]?.id ?? null;
      if (sel && selectedRef.current == null) setSelectedId(sel);
    } catch (err) {
      console.error("checkpoints refresh failed", err);
    }
  }, []);

  // Refetch on every appended event (auto-captures arrive this way).
  useEffect(() => {
    void refresh();
    const unsubscribe = streamEvents(() => void refresh(), 0);
    return unsubscribe;
  }, [refresh]);

  // Load the selected checkpoint's diff whenever the selection changes.
  useEffect(() => {
    const id = projectRef.current;
    if (!id || !selectedId) {
      setDiff("");
      return;
    }
    void getCheckpointDiff(id, selectedId)
      .then((r) => setDiff(r.diff))
      .catch(() => setDiff(""));
  }, [selectedId, checkpoints]);

  async function onCapture() {
    const id = projectRef.current;
    if (!id) return;
    setBusy(true);
    try {
      await captureCheckpoint(id, "manual");
      await refresh();
    } catch (err) {
      console.error("capture failed", err);
    } finally {
      setBusy(false);
    }
  }

  async function onRevert() {
    const id = projectRef.current;
    if (!id || !selectedId) return;
    if (!confirm("Revert the workspace to this checkpoint? Uncommitted changes after it are lost.")) {
      return;
    }
    setBusy(true);
    try {
      await revertCheckpoint(id, selectedId);
    } catch (err) {
      console.error("revert failed", err);
    } finally {
      setBusy(false);
    }
  }

  return (
    <section
      className={cn(
        "w-full max-w-2xl rounded-xl border border-white/10 bg-white/[0.03] p-5",
        className,
      )}
    >
      <header className="mb-4 flex items-center justify-between">
        <h2 className="text-sm font-semibold text-white/80">Checkpoints</h2>
        <div className="flex items-center gap-3">
          <span className="text-xs text-white/30">
            {projectId ? `${checkpoints.length} snapshot(s)` : "no project"}
          </span>
          <Button onClick={onCapture} disabled={busy || !projectId} className="h-7 px-2 text-xs">
            Capture
          </Button>
        </div>
      </header>

      {!projectId ? (
        <p className="text-sm text-white/30">
          No project yet. Create one to capture checkpoints.
        </p>
      ) : checkpoints.length === 0 ? (
        <p className="text-sm text-white/30">
          No checkpoints yet. They are captured automatically after each turn.
        </p>
      ) : (
        <div className="flex gap-4">
          <ul className="w-44 shrink-0 space-y-1">
            {checkpoints.map((cp) => (
              <li key={cp.id}>
                <button
                  onClick={() => setSelectedId(cp.id)}
                  className={cn(
                    "w-full truncate rounded-md px-2 py-1.5 text-left text-xs",
                    cp.id === selectedId
                      ? "bg-white/10 text-white"
                      : "text-white/60 hover:bg-white/5",
                  )}
                >
                  <span className="block truncate">{cp.label || "checkpoint"}</span>
                  <span className="text-white/30">
                    {cp.files_changed} file(s) ·{" "}
                    <span className="text-emerald-400/70">+{cp.insertions}</span>{" "}
                    <span className="text-red-400/70">-{cp.deletions}</span>
                  </span>
                </button>
              </li>
            ))}
          </ul>

          <div className="min-w-0 flex-1 space-y-2">
            <div className="flex justify-end">
              <Button
                onClick={onRevert}
                disabled={busy || !selectedId}
                className="h-7 bg-red-500/15 px-2 text-xs text-red-200 hover:bg-red-500/25"
              >
                Revert to this
              </Button>
            </div>
            <pre className="max-h-72 overflow-auto rounded-md border border-white/10 bg-black/40 p-3 font-mono text-xs leading-relaxed">
              {diff ? <DiffView diff={diff} /> : <span className="text-white/30">No diff.</span>}
            </pre>
          </div>
        </div>
      )}
    </section>
  );
}

/** Colorize a unified diff: green additions, red deletions, dim metadata. */
function DiffView({ diff }: { diff: string }) {
  return (
    <>
      {diff.split("\n").map((line, i) => {
        const cls = line.startsWith("+")
          ? "text-emerald-300"
          : line.startsWith("-")
            ? "text-red-300"
            : line.startsWith("@@")
              ? "text-sky-300"
              : line.startsWith("diff ") || line.startsWith("index ")
                ? "text-white/30"
                : "text-white/60";
        return (
          <div key={i} className={cls}>
            {line || " "}
          </div>
        );
      })}
    </>
  );
}

"use client";

import { useEffect, useMemo, useRef, useState } from "react";

import { streamEvents, type StoredEvent } from "@/lib/api/commands";
import {
  interruptSession,
  sendTurn,
  startSession,
  stopSession,
  type Harness,
} from "@/lib/api/sessions";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

const HARNESSES: { id: Harness; label: string }[] = [
  { id: "codex", label: "Codex" },
  { id: "claude", label: "Claude Code" },
];

/**
 * A minimal harness switch tester: pick the agent (Codex or Claude), point it
 * at a workspace, start a session, send turns, and watch its events stream back
 * live. Proves the per-session harness selection wired through the Rust backend
 * (PRD FR1, §4).
 */
export function HarnessLauncher({ className }: { className?: string }) {
  const [harness, setHarness] = useState<Harness>("codex");
  const [workspace, setWorkspace] = useState("");
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [activeHarness, setActiveHarness] = useState<Harness | null>(null);
  const [turn, setTurn] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [events, setEvents] = useState<StoredEvent[]>([]);
  const logRef = useRef<HTMLDivElement>(null);

  // Subscribe to the live log once; we filter to the active session below.
  useEffect(() => {
    const unsubscribe = streamEvents((event) => {
      setEvents((prev) =>
        prev.some((e) => e.seq === event.seq) ? prev : [...prev, event],
      );
    }, 0);
    return unsubscribe;
  }, []);

  // Only show events for the running session.
  const sessionEvents = useMemo(() => {
    if (!sessionId) return [];
    return events.filter(
      (e) =>
        (e.payload as { session_id?: string } | null)?.session_id === sessionId,
    );
  }, [events, sessionId]);

  useEffect(() => {
    logRef.current?.scrollTo({ top: logRef.current.scrollHeight });
  }, [sessionEvents]);

  async function run<T>(fn: () => Promise<T>): Promise<T | undefined> {
    setBusy(true);
    setError(null);
    try {
      return await fn();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      return undefined;
    } finally {
      setBusy(false);
    }
  }

  async function onStart() {
    const res = await run(() =>
      startSession({
        harness,
        workspace: workspace.trim() || undefined,
      }),
    );
    if (res) {
      setSessionId(res.session_id);
      setActiveHarness(res.harness);
    }
  }

  async function onSend() {
    if (!sessionId || !turn.trim()) return;
    const text = turn.trim();
    const res = await run(() => sendTurn(sessionId, text));
    if (res) setTurn("");
  }

  async function onInterrupt() {
    if (sessionId) await run(() => interruptSession(sessionId));
  }

  async function onStop() {
    if (!sessionId) return;
    await run(() => stopSession(sessionId));
    setSessionId(null);
    setActiveHarness(null);
  }

  return (
    <section
      className={cn(
        "w-full max-w-2xl rounded-xl border border-white/10 bg-white/[0.03] p-5",
        className,
      )}
    >
      <header className="mb-4 flex items-center justify-between">
        <h2 className="text-sm font-semibold text-white/80">Harness tester</h2>
        {sessionId ? (
          <span className="rounded-full bg-emerald-500/15 px-2 py-0.5 text-xs text-emerald-300">
            {activeHarness} · {sessionId.slice(0, 8)}
          </span>
        ) : (
          <span className="rounded-full bg-white/10 px-2 py-0.5 text-xs text-white/50">
            no session
          </span>
        )}
      </header>

      {/* Harness picker */}
      <div className="mb-3 flex gap-2">
        {HARNESSES.map((h) => (
          <button
            key={h.id}
            type="button"
            disabled={!!sessionId}
            onClick={() => setHarness(h.id)}
            className={cn(
              "flex-1 rounded-md border px-3 py-2 text-sm transition",
              harness === h.id
                ? "border-emerald-400/60 bg-emerald-500/15 text-emerald-200"
                : "border-white/10 bg-black/30 text-white/60 hover:border-white/30",
              sessionId && "cursor-not-allowed opacity-50",
            )}
          >
            {h.label}
          </button>
        ))}
      </div>

      {/* Start a session, or drive the running one */}
      {!sessionId ? (
        <div className="flex gap-2">
          <input
            value={workspace}
            onChange={(e) => setWorkspace(e.target.value)}
            placeholder="workspace path (optional)"
            className="flex-1 rounded-md border border-white/10 bg-black/30 px-3 py-2 text-sm text-white outline-none placeholder:text-white/30 focus:border-white/30"
          />
          <Button onClick={onStart} disabled={busy}>
            {busy ? "Starting…" : "Start"}
          </Button>
        </div>
      ) : (
        <div className="flex gap-2">
          <input
            value={turn}
            onChange={(e) => setTurn(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && onSend()}
            placeholder="send a turn…"
            className="flex-1 rounded-md border border-white/10 bg-black/30 px-3 py-2 text-sm text-white outline-none placeholder:text-white/30 focus:border-white/30"
          />
          <Button onClick={onSend} disabled={busy}>
            Send
          </Button>
          <Button onClick={onInterrupt} disabled={busy} variant="secondary">
            Interrupt
          </Button>
          <Button onClick={onStop} disabled={busy} variant="secondary">
            Stop
          </Button>
        </div>
      )}

      {error && <p className="mt-2 text-xs text-red-300">{error}</p>}

      <div
        ref={logRef}
        className="mt-4 max-h-64 overflow-y-auto rounded-md border border-white/10 bg-black/40 p-3 font-mono text-xs"
      >
        {sessionEvents.length === 0 ? (
          <p className="text-white/30">
            {sessionId
              ? "No events yet. Send a turn."
              : "Start a session to drive an agent."}
          </p>
        ) : (
          sessionEvents.map((event) => (
            <div key={event.seq} className="flex gap-3 py-0.5">
              <span className="text-white/30">#{event.seq}</span>
              <span className="text-emerald-300">{event.kind}</span>
              <span className="truncate text-white/50">
                {JSON.stringify(event.payload)}
              </span>
            </div>
          ))
        )}
      </div>
    </section>
  );
}

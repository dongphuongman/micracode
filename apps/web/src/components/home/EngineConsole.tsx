"use client";

import { useEffect, useRef, useState } from "react";

import {
  dispatchCommand,
  streamEvents,
  type StoredEvent,
} from "@/lib/api/commands";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

/**
 * A minimal console over the event-sourced core: type a command kind, dispatch
 * it through the Rust engine, and watch the resulting events stream back live
 * over SSE. This is a thin proof that the frontend drives the new backend.
 */
export function EngineConsole({ className }: { className?: string }) {
  const [kind, setKind] = useState("send_turn");
  const [events, setEvents] = useState<StoredEvent[]>([]);
  const [status, setStatus] = useState<"connecting" | "live" | "error">(
    "connecting",
  );
  const [busy, setBusy] = useState(false);
  const logRef = useRef<HTMLDivElement>(null);

  // Subscribe to the live stream once, replaying the log from the start.
  useEffect(() => {
    let stopped = false;
    setStatus("connecting");
    const unsubscribe = streamEvents((event) => {
      if (stopped) return;
      setStatus("live");
      setEvents((prev) =>
        prev.some((e) => e.seq === event.seq) ? prev : [...prev, event],
      );
    }, 0);
    return () => {
      stopped = true;
      unsubscribe();
    };
  }, []);

  // Keep the log scrolled to the newest event.
  useEffect(() => {
    logRef.current?.scrollTo({ top: logRef.current.scrollHeight });
  }, [events]);

  async function onDispatch() {
    if (!kind.trim()) return;
    setBusy(true);
    try {
      await dispatchCommand({
        id: crypto.randomUUID(),
        kind: kind.trim(),
        payload: { at: new Date().toISOString() },
      });
      // No need to append here — the event arrives over the live stream.
    } catch (err) {
      setStatus("error");
      console.error("dispatch failed", err);
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
        <h2 className="text-sm font-semibold text-white/80">Engine console</h2>
        <span
          className={cn(
            "rounded-full px-2 py-0.5 text-xs",
            status === "live" && "bg-emerald-500/15 text-emerald-300",
            status === "connecting" && "bg-amber-500/15 text-amber-300",
            status === "error" && "bg-red-500/15 text-red-300",
          )}
        >
          {status}
        </span>
      </header>

      <div className="flex gap-2">
        <input
          value={kind}
          onChange={(e) => setKind(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && onDispatch()}
          placeholder="command kind, e.g. send_turn"
          className="flex-1 rounded-md border border-white/10 bg-black/30 px-3 py-2 text-sm text-white outline-none placeholder:text-white/30 focus:border-white/30"
        />
        <Button onClick={onDispatch} disabled={busy}>
          {busy ? "Dispatching…" : "Dispatch"}
        </Button>
      </div>

      <div
        ref={logRef}
        className="mt-4 max-h-64 overflow-y-auto rounded-md border border-white/10 bg-black/40 p-3 font-mono text-xs"
      >
        {events.length === 0 ? (
          <p className="text-white/30">No events yet. Dispatch a command.</p>
        ) : (
          events.map((event) => (
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

"use client";

import { useCallback, useEffect, useRef, useState } from "react";

import { streamEvents } from "@/lib/api/commands";
import {
  getThread,
  listThreads,
  type Message,
  type Thread,
  type ThreadSummary,
} from "@/lib/api/threads";
import { cn } from "@/lib/utils";

/**
 * A live view of the event-sourced read model (PRD FR2): the threads the Rust
 * core folds out of its event log, rendered as turns and messages. The SSE
 * event stream is used purely as a "something changed" signal — each tick
 * refetches the projection, so the panel always reflects the latest fold
 * without re-deriving it in the browser.
 */
export function ThreadsPanel({ className }: { className?: string }) {
  const [threads, setThreads] = useState<ThreadSummary[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [thread, setThread] = useState<Thread | null>(null);
  const selectedRef = useRef<string | null>(null);
  selectedRef.current = selectedId;

  const refresh = useCallback(async () => {
    try {
      const summaries = await listThreads();
      setThreads(summaries);
      const id = selectedRef.current ?? summaries[0]?.id ?? null;
      if (id) {
        if (selectedRef.current == null) setSelectedId(id);
        setThread(await getThread(id));
      }
    } catch (err) {
      console.error("threads refresh failed", err);
    }
  }, []);

  // Refetch on every appended event (and once on mount via the replay).
  useEffect(() => {
    void refresh();
    const unsubscribe = streamEvents(() => void refresh(), 0);
    return unsubscribe;
  }, [refresh]);

  // Load the selected thread immediately on selection change.
  useEffect(() => {
    if (selectedId) void getThread(selectedId).then(setThread).catch(() => {});
  }, [selectedId]);

  return (
    <section
      className={cn(
        "w-full max-w-2xl rounded-xl border border-white/10 bg-white/[0.03] p-5",
        className,
      )}
    >
      <header className="mb-4 flex items-center justify-between">
        <h2 className="text-sm font-semibold text-white/80">Threads</h2>
        <span className="text-xs text-white/30">{threads.length} session(s)</span>
      </header>

      {threads.length === 0 ? (
        <p className="text-sm text-white/30">
          No threads yet. Start a session to drive Claude.
        </p>
      ) : (
        <div className="flex gap-4">
          <ul className="w-40 shrink-0 space-y-1">
            {threads.map((t) => (
              <li key={t.id}>
                <button
                  onClick={() => setSelectedId(t.id)}
                  className={cn(
                    "w-full truncate rounded-md px-2 py-1.5 text-left text-xs",
                    t.id === selectedId
                      ? "bg-white/10 text-white"
                      : "text-white/60 hover:bg-white/5",
                  )}
                >
                  <span className="block truncate font-mono">{t.id.slice(0, 8)}</span>
                  <span className="text-white/30">
                    {t.turn_count} turn(s) · {t.status}
                  </span>
                </button>
              </li>
            ))}
          </ul>

          <div className="min-w-0 flex-1 space-y-3">
            {thread?.turns.length ? (
              thread.turns.map((turn) => (
                <div key={turn.index} className="space-y-1.5">
                  {turn.messages.map((message, i) => (
                    <MessageRow key={i} message={message} />
                  ))}
                </div>
              ))
            ) : (
              <p className="text-sm text-white/30">No turns in this thread yet.</p>
            )}
          </div>
        </div>
      )}
    </section>
  );
}

function MessageRow({ message }: { message: Message }) {
  if (message.role === "tool") {
    return (
      <div className="rounded-md border border-white/10 bg-black/30 px-3 py-2 font-mono text-xs">
        <div className="text-amber-300">⚙ {message.name}</div>
        {message.result != null && (
          <div
            className={cn(
              "mt-1 truncate",
              message.is_error ? "text-red-300" : "text-white/50",
            )}
          >
            {message.result}
          </div>
        )}
      </div>
    );
  }

  const isUser = message.role === "user";
  return (
    <div
      className={cn(
        "rounded-md px-3 py-2 text-sm",
        isUser ? "bg-white/[0.06] text-white/90" : "bg-emerald-500/[0.08] text-white/80",
      )}
    >
      <span className="mr-2 text-xs uppercase tracking-wide text-white/30">
        {message.role}
      </span>
      {message.text}
    </div>
  );
}

import { createResource, createSignal, Match, onMount, Show, Switch } from "solid-js";

import { commands } from "./ipc/bindings";
import "./App.css";

export default function App() {
  const [info] = createResource(() => commands.appInfo());
  const [readyMs, setReadyMs] = createSignal<number | null>(null);
  const [readyFailed, setReadyFailed] = createSignal(false);

  onMount(() => {
    // Two frames: the first schedules the paint, the second runs after it has
    // happened. Marking ready any earlier would measure "DOM built", not
    // "user can see and use the window".
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        commands.markReady().then(setReadyMs, (error: unknown) => {
          // Never a bare `void promise`: an unhandled rejection here would
          // leave the UI showing "measuring..." forever with nothing logged.
          console.error("deepslate: mark_ready failed", error);
          setReadyFailed(true);
        });
      });
    });
  });

  return (
    <div class="shell">
      <header class="titlebar">
        <div class="mark" aria-hidden="true" />
        <span class="wordmark">Deepslate</span>
      </header>

      <main class="body">
        <section class="panel">
          <div class="panel-head">
            <h1 class="panel-title">Milestone 0 — walking skeleton</h1>
            <p class="panel-sub">
              Window, typed IPC round trip, and startup measurement.
            </p>
          </div>

          <dl class="rows">
            {/*
              Order matters. Calling a Solid resource accessor after it has
              errored RE-THROWS the error, which breaks the reactive subtree and
              freezes the UI on its last render. So the error case is matched
              first and `info()` is never called on that path.
            */}
            <Switch
              fallback={
                <div class="row">
                  <dt>IPC round trip</dt>
                  <dd>calling…</dd>
                </div>
              }
            >
              <Match when={info.error}>
                <div class="row">
                  <dt>IPC round trip</dt>
                  <dd class="bad">
                    failed — {String((info.error as Error)?.message ?? info.error)}
                  </dd>
                </div>
              </Match>

              <Match when={info()}>
                {(loaded) => (
                  <>
                    <div class="row">
                      <dt>IPC round trip</dt>
                      <dd class="ok">ok — types generated from Rust</dd>
                    </div>
                    <div class="row">
                      <dt>Crate</dt>
                      <dd>
                        {loaded().name} v{loaded().version}
                      </dd>
                    </div>
                    <div class="row">
                      <dt>Target</dt>
                      <dd>{loaded().target}</dd>
                    </div>
                    <div class="row">
                      <dt>Backend uptime</dt>
                      <dd>{loaded().uptimeMs} ms at call time</dd>
                    </div>
                  </>
                )}
              </Match>
            </Switch>

            <div class="row">
              <dt>Cold start</dt>
              <dd>
                <Show
                  when={readyMs() !== null}
                  fallback={
                    readyFailed() ? (
                      <span class="bad">unavailable — IPC failed</span>
                    ) : (
                      "measuring…"
                    )
                  }
                >
                  <span class={(readyMs() ?? 0) <= 2000 ? "ok" : "bad"}>
                    {readyMs()} ms
                  </span>{" "}
                  / 2000 ms budget
                </Show>
              </dd>
            </div>
          </dl>
        </section>
      </main>
    </div>
  );
}

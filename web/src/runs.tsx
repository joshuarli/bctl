import { Badge, Button, EmptyState, Notice, PageHeader, Panel, type Tone } from "@laputa-systems/ui-dashboard";
import { For, Show, createSignal, onCleanup, onSettled } from "solid-js";
import {
  artifactUrl, authorityText, automation, control, resumeAutomation, resumeRun, run, runArtifacts, runLog, sessionRuns, takeControl,
  type Artifacts, type Control, type Run, type SessionView
} from "./api.ts";
import { navigate } from "./app.tsx";
import { Viewer, type Stream } from "./session.tsx";

const POLL_MS = 1500;
const LOG_KEEP_CHARS = 256 * 1024;

export function runTone(state: string): Tone {
  switch (state) {
    case "running": case "starting": return "info";
    case "needs-interaction": case "interrupted": return "warn";
    case "succeeded": return "ok";
    case "failed": return "bad";
    default: return "neutral";
  }
}

function when(ms: number): string {
  return new Date(ms).toLocaleString();
}

function link(path: string, label: string) {
  return <a href={path} onClick={(e) => { e.preventDefault(); navigate(path); }}>{label}</a>;
}

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export function RunsPanel(props: { session: string }) {
  const [runs, setRuns] = createSignal<Run[] | null>(null);
  const [problem, setProblem] = createSignal<string | null>(null);
  onSettled(() => {
    let alive = true;
    const load = async () => {
      try {
        const next = await sessionRuns(props.session);
        if (alive) { setRuns(next); setProblem(null); }
      } catch (e) {
        if (alive) setProblem(message(e));
      }
    };
    void load();
    const timer = setInterval(() => void load(), POLL_MS);
    onCleanup(() => { alive = false; clearInterval(timer); });
  });
  return <Panel title="Runs" flush>
    <Show when={problem()}>{(p) => <Notice tone="bad">{p()}</Notice>}</Show>
    <Show when={runs()}>{(all) =>
      <Show when={all().length > 0} fallback={<EmptyState title="No runs" detail="Start one with `bctl run` or `bctl grab`." />}>
        <table class="ui-table runs" data-testid="runs">
          <thead><tr><th>Run</th><th>Job</th><th>State</th><th>Outcome</th><th>Started</th></tr></thead>
          <tbody>
            <For each={all()}>{(r) =>
              <tr>
                <td>{link(`/runs/${r.id}`, `#${r.id}`)}</td>
                <td>{r.job ?? r.kind}</td>
                <td><Badge tone={runTone(r.state)} dot>{r.state}</Badge></td>
                <td>{r.outcome ?? "–"}</td>
                <td>{when(r.created_ms)}</td>
              </tr>
            }</For>
          </tbody>
        </table>
      </Show>
    }</Show>
  </Panel>;
}

function useRun(id: number) {
  const [current, setCurrent] = createSignal<Run | null>(null);
  const [problem, setProblem] = createSignal<string | null>(null);
  onSettled(() => {
    let alive = true;
    const load = async () => {
      try {
        const next = await run(id);
        if (alive) { setCurrent(next); setProblem(null); }
      } catch (e) {
        if (alive) setProblem(message(e));
      }
    };
    void load();
    const timer = setInterval(() => void load(), POLL_MS);
    onCleanup(() => { alive = false; clearInterval(timer); });
  });
  return { current, problem, reload: async () => { setCurrent(await run(id)); } };
}

function LogPanel(props: { id: number }) {
  const [text, setText] = createSignal("");
  const [clipped, setClipped] = createSignal(false);
  const [problem, setProblem] = createSignal<string | null>(null);
  onSettled(() => {
    let alive = true;
    let next: number | null = null;
    const load = async () => {
      try {
        const chunk = await runLog(props.id, next);
        if (!alive) return;
        if (next === null && chunk.start > 0) setClipped(true);
        next = chunk.end;
        if (chunk.text) {
          setText((old) => {
            const joined = old + chunk.text;
            if (joined.length <= LOG_KEEP_CHARS) return joined;
            setClipped(true);
            return joined.slice(joined.length - LOG_KEEP_CHARS);
          });
        }
        setProblem(null);
      } catch (e) {
        if (alive) setProblem(message(e));
      }
    };
    void load();
    const timer = setInterval(() => void load(), POLL_MS);
    onCleanup(() => { alive = false; clearInterval(timer); });
  });
  return <Panel title="Log" meta={<Show when={clipped()}><span>earlier output not shown</span></Show>}>
    <Show when={problem()}>{(p) => <Notice tone="bad">{p()}</Notice>}</Show>
    <pre class="log" data-testid="log">{text() || "The log is empty."}</pre>
  </Panel>;
}

function ArtifactsPanel(props: { id: number; ended: boolean }) {
  const [list, setList] = createSignal<Artifacts | null>(null);
  const [problem, setProblem] = createSignal<string | null>(null);
  onSettled(() => {
    let alive = true;
    const load = async () => {
      try {
        const next = await runArtifacts(props.id);
        if (alive) { setList(next); setProblem(null); }
      } catch (e) {
        if (alive) setProblem(message(e));
      }
    };
    void load();
    const timer = setInterval(() => void load(), POLL_MS * 2);
    onCleanup(() => { alive = false; clearInterval(timer); });
  });
  return <Panel title="Artifacts" meta={<span>downloads only</span>} flush>
    <Show when={problem()}>{(p) => <Notice tone="bad">{p()}</Notice>}</Show>
    <Show when={list()}>{(all) =>
      <Show when={all().files.length > 0} fallback={<EmptyState title="No files yet" detail={props.ended ? "The run wrote nothing." : "The run has not written anything yet."} />}>
        <ul class="artifacts" data-testid="artifacts">
          <For each={all().files}>{(file) =>
            <li><a href={artifactUrl(props.id, file.path)} download={file.path.split("/").pop()}>{file.path}</a><span>{file.size} B</span></li>
          }</For>
        </ul>
        <Show when={all().truncated}><Notice tone="warn">Only the first files are listed; the rest are in the output directory.</Notice></Show>
      </Show>
    }</Show>
  </Panel>;
}

function Takeover(props: { run: Run; session: SessionView | null; reload: () => Promise<void> }) {
  const [lease, setLease] = createSignal<Control>(automation);
  const [stream, setStream] = createSignal<Stream>("connecting");
  const [problem, setProblem] = createSignal<string | null>(null);
  const name = () => props.run.session;
  onSettled(() => {
    let alive = true;
    const load = async () => {
      try {
        const next = await control(name());
        if (alive) setLease(next);
      } catch (e) {
        if (alive) setProblem(message(e));
      }
    };
    void load();
    const timer = setInterval(() => void load(), POLL_MS);
    onCleanup(() => { alive = false; clearInterval(timer); });
  });
  const act = async (what: () => Promise<void>) => {
    try { setProblem(null); await what(); } catch (e) { setProblem(message(e)); }
  };
  const waiting = () => props.run.state === "needs-interaction";
  const resume = async () => {
    if (waiting()) {
      await resumeRun(props.run.id);
      setLease(await control(name()));
    } else {
      setLease(await resumeAutomation(name()));
    }
    await props.reload();
  };
  return <Panel title="Live view" meta={<span data-testid="stream">{stream() === "live" ? "live" : stream() === "lost" ? "stream lost" : "connecting"}</span>}>
    <Show when={problem()}>{(p) => <Notice tone="bad">{p()}</Notice>}</Show>
    <Show when={waiting() && props.run.interaction}>{(i) =>
      <Notice tone="warn">The run needs you: {i().reason} at {i().url}. Take control, open that address in the browser, sign in, then resume automation.</Notice>
    }</Show>
    <p data-testid="authority">{authorityText(lease(), waiting())}</p>
    <div class="controls">
      <Show when={!lease().mine}>
        <Button disabled={lease().authority !== "automation" || props.session?.input !== "interactive"} onClick={() => void act(async () => { setLease(await takeControl(name())); })}>Take control</Button>
      </Show>
      <Show when={lease().mine || waiting()}>
        <Button disabled={lease().authority !== "automation" && !lease().mine} onClick={() => void act(resume)}>Resume automation</Button>
      </Show>
    </div>
    <Show when={props.session?.state === "ready"} fallback={<EmptyState title="The session is not running" detail="There is no live view." />}>
      <Show when={props.session?.generation} keyed>{(generation) =>
        <Viewer name={name()} generation={generation} viewOnly={!(lease().mine && lease().authority === "human")} onStream={setStream} />
      }</Show>
    </Show>
  </Panel>;
}

export function RunPage(props: { id: number; sessions: SessionView[] }) {
  const { current, problem, reload } = useRun(props.id);
  return <>
    <Show when={problem() && !current()}>{(p) => <EmptyState title={`No run ${props.id}`} detail={p()} />}</Show>
    <Show when={current()}>{(r) => <>
      <PageHeader title={`Run #${r().id}`} titleExtra={<Badge tone={runTone(r().state)} dot>{r().state}</Badge>}>
        <Button onClick={() => navigate(`/sessions/${encodeURIComponent(r().session)}`)}>Session {r().session}</Button>
      </PageHeader>
      <Show when={problem()}>{(p) => <Notice tone="bad">{p()}</Notice>}</Show>
      <div class="session-layout">
        <div class="session-side">
          <Show when={["starting", "running", "needs-interaction"].includes(r().state)}>
            <Takeover run={r()} session={props.sessions.find((s) => s.name === r().session) ?? null} reload={reload} />
          </Show>
          <LogPanel id={r().id} />
        </div>
        <div class="session-side">
          <Panel title="Status">
            <dl class="facts">
              <dt>Outcome</dt><dd data-testid="outcome">{r().outcome ?? "–"}</dd>
              <Show when={r().detail}><dt>Detail</dt><dd>{r().detail}</dd></Show>
              <dt>Kind</dt><dd>{r().job ? `${r().kind}, job ${r().job}` : r().kind}</dd>
              <Show when={r().progress}>{(p) => <>
                <dt>Progress</dt><dd data-testid="progress">{p().pages_done} pages done, {p().pages_queued} queued, {p().assets_done} assets</dd>
              </>}</Show>
            </dl>
          </Panel>
          <Panel title="Attempts" flush>
            <table class="ui-table">
              <thead><tr><th>#</th><th>State</th><th>Outcome</th><th>Started</th></tr></thead>
              <tbody>
                <For each={r().attempts}>{(a) =>
                  <tr title={a.detail ?? undefined}><td>{a.number}</td><td>{a.state}</td><td>{a.outcome ?? "–"}</td><td>{when(a.started_ms)}</td></tr>
                }</For>
              </tbody>
            </table>
          </Panel>
          <Panel title="Inputs">
            <dl class="facts">
              <dt>Session</dt><dd>{link(`/sessions/${encodeURIComponent(r().session)}`, r().session)}</dd>
              <dt>Profile</dt><dd>{r().profile}</dd>
              <dt>Image</dt><dd>{r().image}</dd>
              <dt>Image digest</dt><dd>{r().image_digest}</dd>
              <dt>Browser</dt><dd>{r().browser_version}</dd>
              <dt>Input digest</dt><dd>{r().input_digest}</dd>
              <dt>Output</dt><dd>{r().output_dir}</dd>
            </dl>
          </Panel>
          <Panel title="Resolved config">
            <pre class="log" data-testid="config">{JSON.stringify(r().config, null, 2)}</pre>
          </Panel>
          <ArtifactsPanel id={r().id} ended={!["starting", "running", "needs-interaction", "paused"].includes(r().state)} />
        </div>
      </div>
    </>}</Show>
  </>;
}

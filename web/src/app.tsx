import { Badge, EmptyState, ErrorState, PageHeader, Panel, Shell, type Tone } from "@laputa-systems/ui-dashboard";
import { For, Show, createSignal, onCleanup, onSettled } from "solid-js";
import { SignedOut, sessions, type SessionView } from "./api.ts";
import { RunPage } from "./runs.tsx";
import { SessionPage } from "./session.tsx";

const POLL_MS = 2000;

export function stateTone(state: SessionView["state"]): Tone {
  switch (state) {
    case "ready": return "ok";
    case "starting": return "info";
    case "stopping": return "warn";
    case "failing": return "bad";
    case "failed": return "bad";
    case "stopped": return "neutral";
  }
}

export function navigate(path: string): void {
  history.pushState(null, "", path);
  window.dispatchEvent(new PopStateEvent("popstate"));
}

function useSessions() {
  const [list, setList] = createSignal<SessionView[] | null>(null);
  const [error, setError] = createSignal<Error | null>(null);
  onSettled(() => {
    let alive = true;
    const load = async () => {
      try {
        const next = await sessions();
        if (alive) { setList(next); setError(null); }
      } catch (e) {
        if (alive) setError(e instanceof Error ? e : new Error(String(e)));
      }
    };
    void load();
    const timer = setInterval(() => void load(), POLL_MS);
    onCleanup(() => { alive = false; clearInterval(timer); });
  });
  return { list, error };
}

function SessionsPage(props: { list: SessionView[] }) {
  return <>
    <PageHeader title="Sessions" />
    <Show when={props.list.length > 0} fallback={
      <EmptyState title="No sessions" detail="Start one with `bctl up NAME`." />
    }>
      <Panel title="Browsers" flush>
        <table class="ui-table sessions">
          <thead><tr><th>Name</th><th>State</th><th>Waiting on</th><th>Profile</th><th>Image</th></tr></thead>
          <tbody>
            <For each={props.list}>{(s) =>
              <tr>
                <td><a href={`/sessions/${encodeURIComponent(s.name)}`} onClick={(e) => { e.preventDefault(); navigate(`/sessions/${encodeURIComponent(s.name)}`); }}>{s.name}</a></td>
                <td><Badge tone={stateTone(s.state)} dot>{s.state}</Badge></td>
                <td>{s.stage ?? (s.failure ? <span class="failure">{s.failure}</span> : "–")}</td>
                <td>{s.profile}</td>
                <td>{s.image}</td>
              </tr>
            }</For>
          </tbody>
        </table>
      </Panel>
    </Show>
  </>;
}

export function App() {
  const [path, setPath] = createSignal(location.pathname);
  const onPop = () => setPath(location.pathname);
  window.addEventListener("popstate", onPop);
  onCleanup(() => window.removeEventListener("popstate", onPop));
  const { list, error } = useSessions();
  const selected = () => {
    const match = /^\/sessions\/([^/]+)$/.exec(path());
    return match?.[1] ? decodeURIComponent(match[1]) : null;
  };
  const selectedRun = () => {
    const match = /^\/runs\/(\d+)$/.exec(path());
    return match?.[1] ? Number(match[1]) : null;
  };
  const nav = () => [{
    label: "bctl",
    items: [{ href: "/", label: "Sessions", current: selected() === null && selectedRun() === null }]
  }];
  return <Shell brand={{ href: "/", name: "bctl" }} nav={nav()}>
    <Show when={!(error() instanceof SignedOut)} fallback={
      <ErrorState title="Signed out" detail="Run `bctl serve --open` to get a new login link." />
    }>
      <Show when={error()}>{(e) => <ErrorState compact detail={`Cannot reach the bctl daemon: ${e().message}`} />}</Show>
      <Show when={list()} fallback={<p role="status">Loading…</p>}>{(all) =>
        <Show when={selectedRun()} fallback={
          <Show when={selected()} fallback={<SessionsPage list={all()} />}>{(name) =>
            <SessionPage name={name()} session={all().find((s) => s.name === name()) ?? null} />
          }</Show>
        }>{(id) => <RunPage id={id()} sessions={all()} />}</Show>
      }</Show>
    </Show>
  </Shell>;
}

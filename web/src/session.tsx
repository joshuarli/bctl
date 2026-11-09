import RFB from "@novnc/novnc";
import { Badge, Button, EmptyState, Notice, PageHeader, Panel } from "@laputa-systems/ui-dashboard";
import { For, Show, createEffect, createSignal, onCleanup, onSettled } from "solid-js";
import { control, focusTab, releaseControl, stop, tabs, takeControl, vncUrl, type Control, type SessionView, type Tab } from "./api.ts";
import { navigate, stateTone } from "./app.tsx";

type Stream = "connecting" | "live" | "lost";

function Viewer(props: { name: string; viewOnly: boolean; generation: number; onStream: (s: Stream) => void }) {
  let host!: HTMLDivElement;
  onSettled(() => {
    props.onStream("connecting");
    const rfb = new RFB(host, vncUrl(props.name));
    createEffect(() => props.viewOnly, (viewOnly) => { rfb.viewOnly = viewOnly; });
    rfb.scaleViewport = true;
    rfb.resizeSession = false;
    rfb.focusOnClick = true;
    rfb.addEventListener("connect", () => props.onStream("live"));
    rfb.addEventListener("disconnect", () => props.onStream("lost"));
    onCleanup(() => rfb.disconnect());
  });
  return <div class="viewer" ref={host} data-testid="viewer" />;
}

export function SessionPage(props: { name: string; session: SessionView | null }) {
  const [stream, setStream] = createSignal<Stream>("connecting");
  const [attempt, setAttempt] = createSignal(0);
  const [tabList, setTabList] = createSignal<Tab[]>([]);
  const [problem, setProblem] = createSignal<string | null>(null);
  const [lease, setLease] = createSignal<Control>({ held: false, mine: false });

  onSettled(() => {
    let alive = true;
    const load = async () => {
      if (props.session?.state !== "ready") return;
      try {
        const [nextTabs, nextLease] = await Promise.all([tabs(props.name), control(props.name)]);
        if (alive) { setTabList(nextTabs); setLease(nextLease); }
      } catch (e) {
        if (alive) setProblem(e instanceof Error ? e.message : String(e));
      }
    };
    void load();
    const timer = setInterval(() => void load(), 2000);
    onCleanup(() => { alive = false; clearInterval(timer); });
  });

  const act = async (what: () => Promise<void>) => {
    try { setProblem(null); await what(); } catch (e) { setProblem(e instanceof Error ? e.message : String(e)); }
  };

  return <Show when={props.session} fallback={
    <EmptyState title={`No session named ${props.name}`} detail="It may have been removed." />
  }>{(session) => <>
    <PageHeader title={session().name} titleExtra={<Badge tone={stateTone(session().state)} dot>{session().state}</Badge>}>
      <Button onClick={() => navigate("/")}>All sessions</Button>
    </PageHeader>
    <Show when={problem()}>{(p) => <Notice tone="bad">{p()}</Notice>}</Show>
    <div class="session-layout">
      <Panel title="Browser" meta={<span data-testid="stream">{stream() === "live" ? "live" : stream() === "lost" ? "stream lost, the browser may still be running" : "connecting"}</span>}>
        <Show when={session().state === "ready"} fallback={
          <EmptyState title={`The session is ${session().state}`} detail={session().stage ? `Waiting on ${session().stage}.` : session().failure ?? ""} />
        }>
          <Show when={session().input === "view-only"}>
            <Notice tone="warn">This runtime has no input devices: the view is read-only.</Notice>
          </Show>
          <Show when={lease().held && !lease().mine}>
            <Notice tone="info">Another viewer has control. You are watching.</Notice>
          </Show>
          <Show when={`${attempt()}:${session().generation}`} keyed>
            <Viewer name={session().name} generation={session().generation} viewOnly={!lease().mine} onStream={setStream} />
          </Show>
        </Show>
      </Panel>
      <div class="session-side">
        <Panel title="Controls">
          <div class="controls">
            <Show when={session().input === "interactive"}>
              <Show when={lease().mine} fallback={
                <Button onClick={() => void act(async () => { setLease(await takeControl(session().name)); })}>Take control</Button>
              }>
                <Button onClick={() => void act(async () => { setLease(await releaseControl(session().name)); })}>Release control</Button>
              </Show>
            </Show>
            <Button onClick={() => setAttempt((n) => n + 1)}>Reconnect viewer</Button>
            <Button onClick={() => void act(() => stop(session().name))}>Stop session</Button>
          </div>
          <dl class="facts">
            <dt>Profile</dt><dd>{session().profile}</dd>
            <dt>Image</dt><dd>{session().image}</dd>
            <dt>CDP</dt><dd>{session().cdp_url ?? "–"}</dd>
          </dl>
        </Panel>
        <Panel title="Tabs" flush>
          <ul class="tabs">
            <For each={tabList()}>{(tab) =>
              <li>
                <div class="tab-text"><strong>{tab.title || "Untitled"}</strong><span>{tab.url}</span></div>
                <Button variant="small" onClick={() => void act(() => focusTab(session().name, tab.id))}>Focus</Button>
              </li>
            }</For>
          </ul>
        </Panel>
      </div>
    </div>
  </>}</Show>;
}

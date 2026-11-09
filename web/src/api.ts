export type SessionView = {
  name: string;
  profile: string;
  image: string;
  state: "stopped" | "starting" | "ready" | "stopping" | "failed";
  generation: number;
  stage: string | null;
  failure: string | null;
  machine: string | null;
  cdp_url: string | null;
  vnc_port: number | null;
  input: "interactive" | "view-only" | null;
};

export type Tab = { id: string; title: string; url: string };

export class SignedOut extends Error {}

async function request(path: string, init?: RequestInit): Promise<Response> {
  const response = await fetch(path, { credentials: "same-origin", ...init });
  if (response.status === 401) throw new SignedOut("Not signed in. Run `bctl serve --open`.");
  if (!response.ok) throw new Error((await response.text()) || `${response.status} ${response.statusText}`);
  return response;
}

export async function sessions(): Promise<SessionView[]> {
  const body = (await (await request("/api/sessions")).json()) as { sessions: SessionView[] };
  return body.sessions;
}

export async function tabs(session: string): Promise<Tab[]> {
  return (await (await request(`/api/sessions/${encodeURIComponent(session)}/tabs`)).json()) as Tab[];
}

export async function focusTab(session: string, id: string): Promise<void> {
  await request(`/api/sessions/${encodeURIComponent(session)}/tabs/${encodeURIComponent(id)}/activate`, { method: "POST" });
}

export async function stop(session: string): Promise<void> {
  await request(`/api/sessions/${encodeURIComponent(session)}/down`, { method: "POST" });
}

export function vncUrl(session: string): string {
  return `ws://${location.host}/api/sessions/${encodeURIComponent(session)}/vnc`;
}

export type Control = { held: boolean; mine: boolean };

export async function control(session: string): Promise<Control> {
  return (await (await request(`/api/sessions/${encodeURIComponent(session)}/control`)).json()) as Control;
}

export async function takeControl(session: string): Promise<Control> {
  return (await (await request(`/api/sessions/${encodeURIComponent(session)}/control`, { method: "POST" })).json()) as Control;
}

export async function releaseControl(session: string): Promise<Control> {
  return (await (await request(`/api/sessions/${encodeURIComponent(session)}/control/release`, { method: "POST" })).json()) as Control;
}

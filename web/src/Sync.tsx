import { For, Show, createSignal, onMount } from "solid-js";
import { NexusApi } from "./api";
import { forgetDevice, knownDevices, type KnownDevice } from "./devices";
import type { NexusStore } from "./state";

interface SyncState { device_id: string; device_name: string; peers: { peer_id: string; table: string; last_synced_at: string | null }[] }
interface FileChange { space_id: string; name: string; hash: string; size: number }
interface Changeset { device_id: string; files: FileChange[]; rows: unknown[]; tombstones: unknown[] }

function saveBlob(blob: Blob, name: string) {
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a"); link.href = url; link.download = name; link.click();
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
}

export function SyncPage(props: { store: NexusStore }) {
  const [state, setState] = createSignal<SyncState>();
  const [devices, setDevices] = createSignal(knownDevices());
  const [url, setUrl] = createSignal("");
  const [token, setToken] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [status, setStatus] = createSignal("");
  const [error, setError] = createSignal("");
  const [warnings, setWarnings] = createSignal<string[]>([]);
  async function refresh() {
    try { setState(await props.store.api.request<SyncState>("/v1/sync/state")); }
    catch (error) { setError(String(error)); }
  }
  onMount(() => void refresh());
  function choose(device: KnownDevice) { setUrl(device.url); setToken(device.token ?? ""); }
  async function transfer(from: NexusApi, to: NexusApi, files: FileChange[]) {
    for (const [index, file] of files.entries()) {
      setStatus(`Transferring ${index + 1}/${files.length}: ${file.name}`);
      const query = new URLSearchParams({ space_id: file.space_id, name: file.name, hash: file.hash });
      try {
        try { await to.binary(`/v1/sync/blob?${query}`); continue; } catch { /* Missing or outdated destination blob. */ }
        const blob = await from.binary(`/v1/sync/blob?${query}`);
        await to.request(`/v1/sync/blob?${query}`, { method: "PUT", headers: { "Content-Type": "application/octet-stream" }, body: blob });
      } catch (error) { setWarnings((previous) => [...previous, `${file.name}: ${String(error)}`]); }
    }
  }
  async function sync() {
    if (!url().trim() || !token().trim()) return;
    setBusy(true); setError(""); setWarnings([]); setStatus("Connecting to peer…");
    try {
      const peer = new NexusApi(url().trim(), token().trim());
      const [local, remote] = await Promise.all([props.store.api.request<SyncState>("/v1/sync/state"), peer.request<SyncState>("/v1/sync/state")]);
      if (local.device_id === remote.device_id) throw new Error("Choose a different host to sync with.");
      const outgoing = await props.store.api.request<Changeset>(`/v1/sync?peer_id=${encodeURIComponent(remote.device_id)}`);
      setStatus("Merging changes…");
      const incoming = await peer.request<Changeset>("/v1/sync", { method: "POST", body: JSON.stringify(outgoing) });
      const ack = await props.store.api.request<Changeset>("/v1/sync", { method: "POST", body: JSON.stringify(incoming) });
      // Metadata acknowledgements can advance before blob transfers succeed.
      // Full manifests make unchanged missing blobs retryable on every exchange.
      const [localFiles, remoteFiles] = await Promise.all([
        props.store.api.request<Changeset>("/v1/sync"), peer.request<Changeset>("/v1/sync"),
      ]);
      const remoteHashes = new Map(remoteFiles.files.map((file) => [`${file.space_id}/${file.name}`, file.hash]));
      await transfer(props.store.api, peer, localFiles.files.filter((file) => remoteHashes.get(`${file.space_id}/${file.name}`) === file.hash));
      await transfer(peer, props.store.api, remoteFiles.files);
      await peer.request("/v1/sync", { method: "POST", body: JSON.stringify(ack) });
      setStatus(warnings().length ? "Metadata merged. Some files could not be transferred; details below." : "Sync complete.");
      await refresh(); await props.store.refresh();
    } catch (error) { setError(String(error)); setStatus("Sync stopped. You can retry safely."); }
    finally { setBusy(false); }
  }
  async function exportBundle() {
    setBusy(true); setError("");
    try { saveBlob(await props.store.api.binary("/v1/sync/bundle"), "nexus-sync.zip"); setStatus("Sync bundle downloaded."); }
    catch (error) { setError(String(error)); }
    finally { setBusy(false); }
  }
  async function importBundle(file: File) {
    if (!window.confirm(`Merge the sessions, settings, deletions and files in ${file.name} into this host?`)) return;
    setBusy(true); setError("");
    try {
      if (file.size > 64 * 1024 * 1024) throw new Error("Bundles must be 64 MiB or smaller.");
      const reply = await props.store.api.binary("/v1/sync/bundle", { method: "POST", body: file, headers: { "Content-Type": "application/zip" } });
      saveBlob(reply, "nexus-sync-reply.zip");
      setStatus("Bundle merged. Import the downloaded reply on the sending host to complete the exchange.");
      await refresh(); await props.store.refresh();
    } catch (error) { setError(String(error)); }
    finally { setBusy(false); }
  }
  return <section class="workspace-page sync-page"><h1>Sync and devices</h1><p class="muted">{state()?.device_name} · {state()?.device_id}</p><p>Sync merges all spaces using the host’s existing conflict rules. Provider credentials stay on each host.</p>
    <Show when={error()}><p class="error-banner" role="alert">{error()}</p></Show><p role="status">{status()}</p>
    <fieldset disabled={busy()}><legend>Sync with a host</legend><label>Peer host URL<input type="url" value={url()} placeholder="http://127.0.0.1:8643" onInput={(event) => setUrl(event.currentTarget.value)} /></label><label>Peer bearer token<input type="password" autocomplete="off" value={token()} onInput={(event) => setToken(event.currentTarget.value)} /></label><button disabled={!url() || !token()} onClick={() => void sync()}>Sync now</button></fieldset>
    <Show when={warnings().length}><ul><For each={warnings()}>{(warning) => <li>{warning}</li>}</For></ul></Show>
    <h2>Offline exchange</h2><button disabled={busy()} onClick={() => void exportBundle()}>Download sync bundle</button><label class="upload-button">Import sync bundle<input type="file" accept=".zip" disabled={busy()} onChange={(event) => { const file = event.currentTarget.files?.[0]; if (file) void importBundle(file); event.currentTarget.value = ""; }} /></label>
    <h2>Remembered hosts</h2><For each={devices()} fallback={<p class="muted">No remembered hosts.</p>}>{(device) => <article class="file-row"><div><strong>{device.name}</strong><small>{device.url}</small></div><div><button disabled={busy()} onClick={() => choose(device)}>Use for sync</button><button disabled={busy()} onClick={() => setDevices(forgetDevice(device.hostId))}>Forget access</button></div></article>}</For>
    <h2>Recent sync</h2><For each={state()?.peers ?? []} fallback={<p class="muted">No sync history yet.</p>}>{(peer) => <p class="muted">{peer.peer_id} · {peer.table} · {peer.last_synced_at ?? "never"}</p>}</For>
  </section>;
}

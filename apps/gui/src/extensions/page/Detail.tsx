// One extension, and the approval screen (docs/EXTENSIONS.md §6): what the waiting version
// declares — tools, extra instructions, panel, access — and how it differs from the one running
// now. What the button submits is fixed when the screen opens (`approvalView`), so a pending
// record that moved underneath it reads as stale instead of approving code nobody read; the
// blocks themselves are in panels.ts, where the tests can render them.
//
// Below the approval card: the two switches that belong to the person (enable/disable,
// auto-trust) with what they mean and why they cannot be used, and the logs — the host's own
// errors for the page, and the hub's log file for the extension's tool calls.

import { useEffect, useState } from "react";
import type { ExtensionDiffResponse } from "@bindings/ExtensionDiffResponse";
import type { ExtensionInfo } from "@bindings/ExtensionInfo";
import type { ExtensionLogsResponse } from "@bindings/ExtensionLogsResponse";
import { api, errorText } from "../../api";
import { ErrorBox, Icon } from "../../components";
import { useHubData } from "../../hub";
import { useExtensions } from "../defaults";
import type { ExtensionView } from "../host";
import { ApprovalPanel, Chips, LogsPanel, VersionCard } from "./panels.ts";
import { SafeModeBanner, useSafeMode } from "./SafeMode.ts";
import {
  AUTO_TRUST_HINT,
  approvalView,
  autoTrustBlockedReason,
  badges,
  disableBlockedReason,
  enableState,
  seenAfter,
  shortFingerprint,
} from "./model.ts";

export function ExtensionDetail({ name }: { name: string }) {
  const list = useHubData(() => api.listExtensions().then((r) => r.extensions), [], (e) => e.type === "extension.changed");
  const snapshot = useExtensions();
  const safe = useSafeMode();
  const info = list.data?.find((x) => x.name === name);
  return (
    <div className="page">
      <div className="page-inner ext-detail">
        <a className="ext-back" href="#/extensions">
          ← 所有扩展
        </a>
        <SafeModeBanner state={safe} />
        {info ? (
          <ExtensionBody info={info} page={snapshot.records.get(name)} reload={list.reload} />
        ) : list.error ? (
          <ErrorBox error={list.error} />
        ) : (
          <p className="muted">{list.loading ? "正在读取扩展…" : `hub 里没有叫「${name}」的扩展：它可能被删掉了。`}</p>
        )}
      </div>
    </div>
  );
}

function ExtensionBody({ info, page, reload }: { info: ExtensionInfo; page?: ExtensionView; reload: () => void }) {
  const [seen, setSeen] = useState<string | undefined>(undefined);
  /** A fingerprint this window just approved, so the card does not come back while the list reloads. */
  const [approved, setApproved] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [notice, setNotice] = useState<string>();
  const [diff, setDiff] = useState<ExtensionDiffResponse>();
  const [diffError, setDiffError] = useState<string>();
  const pendingFp = info.pending?.version.fingerprint;
  const requesterId = info.pending?.requested_by;
  const agents = useHubData(() => (requesterId ? api.listAgents().then((r) => r.agents) : Promise.resolve([])), [requesterId], () => false);
  const requester = agents.data?.find((a) => a.id === requesterId)?.name;

  // The fingerprint this screen opened with: it is what the approve button sends. A later
  // pending record (the source changed again) leaves `seen` behind, and the card says so — but
  // one this window just approved is not "seen" at all, or the version after it would read as
  // "the one you looked at is gone" even though the person had approved it (model.ts seenAfter).
  useEffect(() => {
    const next = seenAfter(seen, approved, info);
    if (next !== seen) setSeen(next);
  }, [info, seen, approved]);

  useEffect(() => {
    if (!pendingFp) {
      setDiff(undefined);
      setDiffError(undefined);
      return;
    }
    let live = true;
    setDiff(undefined);
    setDiffError(undefined);
    api.extensionDiff(info.name).then(
      (r) => live && setDiff(r),
      (err) => live && setDiffError(errorText(err)),
    );
    return () => {
      live = false;
    };
  }, [info.name, pendingFp]);

  const view = approvalView(info, seen);
  const enable = enableState(info);
  const disableReason = disableBlockedReason(info);
  const trustReason = autoTrustBlockedReason(info);

  async function run(action: () => Promise<unknown>, done: string) {
    setBusy(true);
    setError(undefined);
    setNotice(undefined);
    try {
      await action();
      setNotice(done);
      reload();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }

  const approve = (fingerprint: string) => {
    void run(async () => {
      await api.enableExtension(info.name, fingerprint);
      // Approved: the card goes at once, rather than waiting for the list to come back (and
      // rather than inviting a second click at a record the hub has already consumed). `seen` is
      // cleared too, so the next version opens as something to read rather than as a replacement.
      setApproved(fingerprint);
      setSeen(undefined);
    }, `已批准并启用 ${shortFingerprint(fingerprint)}。`);
  };
  const disable = () => void run(() => api.disableExtension(info.name), "已停用：它注册的面板、页面和卡片都收了。");
  const reenable = () => {
    if (!("fingerprint" in enable)) return;
    void run(() => api.enableExtension(info.name, enable.fingerprint), "已重新启用。");
  };
  const toggleTrust = () => {
    const on = !info.auto_trust;
    const also = on && info.pending ? "，等待批准的那一版也一并批准并启用了" : "";
    void run(() => api.setExtensionAutoTrust(info.name, on), on ? `已打开自动信任更新${also}。` : "已关闭自动信任更新。");
  };

  return (
    <>
      <header className="ext-head">
        <h1 className="ext-title">{info.name}</h1>
        <Chips badges={badges(info, { safeMode: false, page })} />
        <div className="ext-source mono small" title={info.source}>
          {info.source}
        </div>
        {info.error && (
          <div className="error small" role="alert">
            源目录有问题，hub 没法为它做快照：{info.error}
            {info.enabled && info.current ? "（正在运行的版本照旧，调用不受影响。）" : ""}
          </div>
        )}
      </header>

      <section className="ext-card">
        <div className="ext-card-head">
          <h2>正在运行的一版</h2>
          {!info.current && <span className="muted small">还没有批准过任何版本</span>}
        </div>
        {info.service && <p className="ext-service-status">服务状态：{info.service.state} · 重启次数：{info.service.restarts} · 最近退出原因：{info.service.last_exit ?? "暂无"}</p>}
        {info.current ? <VersionCard version={info.current} /> : <p className="muted small">批准一次之后，调用和界面才用得上它。</p>}
      </section>

      <section className="ext-actions">
        <div className="ext-action">
          <button
            className={info.enabled ? "" : "primary"}
            data-action="enable"
            disabled={busy || !("fingerprint" in enable)}
            title={"reason" in enable ? enable.reason : enable.title}
            onClick={reenable}
          >
            启用
          </button>
          <span className="ext-hint">{"reason" in enable ? enable.reason : enable.title}</span>
        </div>
        <div className="ext-action">
          <button className="ghost danger" data-action="disable" disabled={busy || !!disableReason} title={disableReason} onClick={disable}>
            停用
          </button>
          {disableReason && <span className="ext-hint">{disableReason}</span>}
        </div>
        <div className="ext-action ext-action-trust">
          <button
            role="switch"
            aria-checked={info.auto_trust}
            aria-label={info.auto_trust ? "关闭自动信任更新" : "打开自动信任更新"}
            className={`toggle ${info.auto_trust ? "on" : ""}`}
            data-action="auto-trust"
            disabled={busy || !!trustReason}
            title={trustReason ?? AUTO_TRUST_HINT}
            onClick={toggleTrust}
          >
            <span />
          </button>
          <div className="ext-hint-block">
            <div className="ext-hint-title">自动信任更新</div>
            <div className={`ext-hint ${info.auto_trust ? "on" : ""}`}>{trustReason ?? AUTO_TRUST_HINT}</div>
          </div>
        </div>
      </section>

      {error && <ErrorBox error={error} />}
      {notice && (
        <p className="ext-notice" role="status">
          <Icon name="check" size={14} />
          {notice}
        </p>
      )}

      {info.pending && info.pending.version.fingerprint !== approved ? (
        <ApprovalPanel
          info={info}
          view={view}
          diff={diff}
          diffError={diffError}
          busy={busy}
          requester={requester}
          onApprove={approve}
          onLook={setSeen}
        />
      ) : (
        <section className="ext-card">
          <div className="ext-card-head">
            <h2>待批准的版本</h2>
          </div>
          <p className="muted small">没有。源目录和正在运行的版本一样；改了源目录之后，hub 会在这里放一版等你批准。</p>
        </section>
      )}

      <LogsSection name={info.name} page={page} />
    </>
  );
}

function LogsSection({ name, page }: { name: string; page?: ExtensionView }) {
  const [hub, setHub] = useState<ExtensionLogsResponse>();
  const [error, setError] = useState<string>();
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    let live = true;
    setLoading(true);
    setError(undefined);
    api.extensionLogs(name).then(
      (r) => {
        if (!live) return;
        setHub(r);
        setLoading(false);
      },
      (err) => {
        if (!live) return;
        setError(errorText(err));
        setLoading(false);
      },
    );
    return () => {
      live = false;
    };
  }, [name]);

  const load = () => {
    setLoading(true);
    setError(undefined);
    api.extensionLogs(name).then(
      (r) => {
        setHub(r);
        setLoading(false);
      },
      (err) => {
        setError(errorText(err));
        setLoading(false);
      },
    );
  };

  return <LogsPanel name={name} pageLogs={page?.logs ?? []} hub={hub} hubError={error} loading={loading} onLoad={load} />;
}

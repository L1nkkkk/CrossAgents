const attached = new Set();
const openedByAgent = new Set();
const logs = new Map();
let polling = false;

function pushLog(tabId, level, text, ts) {
  const entries = logs.get(tabId) ?? [];
  entries.push({ level: String(level ?? "log"), text: String(text ?? "").slice(0, 1000), ts: Number(ts) || Date.now() });
  if (entries.length > 150) entries.shift();
  logs.set(tabId, entries);
}

function waitTabComplete(tabId, timeoutMs = 12000) {
  return new Promise((resolve) => {
    let settled = false;
    const finish = () => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      chrome.tabs.onUpdated.removeListener(onUpdated);
      chrome.tabs.onRemoved.removeListener(onRemoved);
      resolve();
    };
    const onUpdated = (id, info) => {
      if (id === tabId && info.status === "complete") finish();
    };
    const onRemoved = (id) => {
      if (id === tabId) finish();
    };
    const timer = setTimeout(finish, timeoutMs);
    chrome.tabs.onUpdated.addListener(onUpdated);
    chrome.tabs.onRemoved.addListener(onRemoved);
    chrome.tabs.get(tabId).then((tab) => {
      if (tab?.status === "complete") finish();
    }).catch(finish);
  });
}

async function execute(command) {
  const tabId = command.tabId;
  switch (command.kind) {
    case "open": {
      const tab = tabId
        ? await chrome.tabs.update(tabId, { url: command.url, active: true })
        : await chrome.tabs.create({ url: command.url, active: true });
      if (!tabId && tab?.id) openedByAgent.add(tab.id);
      if (tab?.id) await waitTabComplete(tab.id, 12000);
      const latest = tab?.id ? await chrome.tabs.get(tab.id).catch(() => tab) : tab;
      return {
        tab: `chrome:${latest.id}`,
        url: latest.url || command.url,
        title: latest.title || latest.url || command.url,
      };
    }
    case "activate": {
      const tab = await chrome.tabs.update(tabId, { active: true });
      await chrome.windows.update(tab.windowId, { focused: true });
      return { ok: true };
    }
    case "close": {
      if (!openedByAgent.has(tabId)) {
        throw new Error("不能关闭非 Agent 打开的外部 Chrome 标签页，避免误关用户的页面");
      }
      await chrome.tabs.remove(tabId);
      openedByAgent.delete(tabId);
      return { ok: true };
    }
    case "navigate":
      if (command.action === "reload") await chrome.tabs.reload(tabId);
      else if (command.action === "back") await chrome.tabs.goBack(tabId);
      else if (command.action === "forward") await chrome.tabs.goForward(tabId);
      else throw new Error("未知的导航操作");
      await waitTabComplete(tabId, 10000);
      return { ok: true };
    case "evaluate":
    case "console":
    case "screenshot": {
      const tab = await chrome.tabs.get(tabId);
      if (!/^https?:\/\//i.test(tab.url || "")) throw new Error("仅可操作 HTTP 或 HTTPS 页面");
      if (!attached.has(tabId)) {
        await chrome.debugger.attach({ tabId }, "1.3");
        attached.add(tabId);
        await chrome.debugger.sendCommand({ tabId }, "Runtime.enable");
      }
      if (command.kind === "console") return logs.get(tabId) ?? [];
      if (command.kind === "screenshot") {
        const image = await chrome.debugger.sendCommand({ tabId }, "Page.captureScreenshot", { format: "png" });
        return { image_base64: image.data, mime: "image/png" };
      }
      const response = await chrome.debugger.sendCommand({ tabId }, "Runtime.evaluate", {
        expression: command.expression, returnByValue: true,
      });
      if (response.exceptionDetails) {
        const detail = response.exceptionDetails.exception?.description || response.exceptionDetails.text || "Chrome 未能执行页面操作";
        throw new Error(detail);
      }
      if (response.result?.value?.error) throw new Error(response.result.value.error);
      return response.result?.value;
    }
    default:
      throw new Error("未知的浏览器操作");
  }
}

async function api(path, body) {
  const response = await fetch(BRIDGE + path, {
    method: "POST",
    headers: { "Content-Type": "application/json", Authorization: "Bearer " + BRIDGE_KEY },
    body: JSON.stringify(body),
  });
  if (!response.ok) throw new Error("CrossAgents 桥接不可用");
  return response.json();
}

async function poll() {
  if (polling) return;
  polling = true;
  while (true) {
    try {
      const tabs = await chrome.tabs.query({});
      const response = await api("/poll", { tabs: tabs.map(({ id, url, title, active }) => ({ id, url, title, active })) });
      for (const command of response.commands) {
        let reply;
        try { reply = { id: command.id, value: await execute(command), ok: true }; }
        catch (error) { reply = { id: command.id, error: error instanceof Error ? error.message : String(error), ok: false }; }
        await api("/reply", reply);
      }
    } catch {
      await new Promise((resolve) => setTimeout(resolve, 2500));
    }
  }
}

chrome.debugger.onEvent.addListener(({ tabId }, method, params) => {
  if (!attached.has(tabId)) return;
  if (method === "Runtime.consoleAPICalled") {
    const text = (params.args ?? []).map(({ value, description }) => String(value ?? description ?? "")).join(" ");
    pushLog(tabId, params.type ?? "log", text, params.timestamp);
  } else if (method === "Runtime.exceptionThrown") {
    const details = params.exceptionDetails || {};
    const text = details.exception?.description || details.text || "Uncaught exception";
    pushLog(tabId, "error", text, params.timestamp);
  }
});
chrome.debugger.onDetach.addListener(({ tabId }) => {
  attached.delete(tabId);
  logs.delete(tabId);
});
chrome.tabs.onRemoved.addListener((tabId) => {
  openedByAgent.delete(tabId);
  attached.delete(tabId);
  logs.delete(tabId);
});
chrome.alarms.create("crossagents-keepalive", { periodInMinutes: 0.5 });
chrome.alarms.onAlarm.addListener(() => void poll());
void poll();

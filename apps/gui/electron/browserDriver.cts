// One browser tab driven through the Chrome DevTools Protocol (`webContents.debugger`, protocol
// 1.3). Its parsing and key tables are in browserCdp.cts. One instance per <webview>, made when
// the tab attaches and kept until it is destroyed (electron/browser.cts).

import type { WebContents } from "electron";
import { axSnapshot, backendNodeFor, consoleEntry, keyDefinition, quadCenter, type AxNode, type ConsoleEntry } from "./browserCdp.cjs";

/** Console entries kept per tab; older ones are dropped. */
const CONSOLE_LIMIT = 200;

export class BrowserTab {
  private refs = new Map<number, number>();
  private logs: ConsoleEntry[] = [];
  /** Resolves once the domains the driver reads are on; rejects if attaching failed. */
  private readonly ready: Promise<void>;

  /** Attaches the debugger; throws when another client (DevTools) already holds it. */
  constructor(private readonly contents: WebContents) {
    contents.debugger.attach("1.3");
    contents.debugger.on("message", (_event, method: string, params: unknown) => this.onMessage(method, params));
    contents.debugger.on("detach", () => this.refs.clear());
    this.ready = (async () => {
      for (const method of ["Accessibility.enable", "DOM.enable", "Runtime.enable"]) await this.send(method);
    })();
    // Read through `ready` by every call; an unhandled rejection here would only be noise.
    this.ready.catch(() => undefined);
  }

  private send<T = any>(method: string, params?: object): Promise<T> {
    return this.contents.debugger.sendCommand(method, params);
  }

  private onMessage(method: string, params: unknown): void {
    const entry = consoleEntry(method, params);
    if (!entry) return;
    this.logs.push(entry);
    if (this.logs.length > CONSOLE_LIMIT) this.logs.splice(0, this.logs.length - CONSOLE_LIMIT);
  }

  /** The page as text; this also makes the refs the actions below take. */
  async snapshot(): Promise<{ text: string; url: string; title: string }> {
    await this.ready;
    const { nodes } = await this.send<{ nodes: AxNode[] }>("Accessibility.getFullAXTree");
    const { text, refs } = axSnapshot(nodes);
    this.refs = refs;
    return { text, url: this.contents.getURL(), title: this.contents.getTitle() };
  }

  /** A click at the centre of the element with `ref`. */
  async click(ref: number): Promise<void> {
    await this.ready;
    const point = await this.pointOf(backendNodeFor(this.refs, ref));
    const button = "left";
    await this.send("Input.dispatchMouseEvent", { type: "mousePressed", ...point, button, clickCount: 1 });
    await this.send("Input.dispatchMouseEvent", { type: "mouseReleased", ...point, button, clickCount: 1 });
  }

  /** Focuses the element with `ref`, types `text`, and presses Enter when `submit`. */
  async type(ref: number, text: string, submit: boolean): Promise<void> {
    await this.ready;
    const backendNodeId = backendNodeFor(this.refs, ref);
    try {
      await this.send("DOM.focus", { backendNodeId });
    } catch {
      throw new Error(`ref=${ref} 不能输入文字（不是输入框），请重新 browser_snapshot`);
    }
    await this.send("Input.insertText", { text });
    if (submit) await this.press("Enter");
  }

  /** One key, pressed and released. `key` is a CDP key name such as `Enter` or `ArrowDown`. */
  async press(key: string): Promise<void> {
    await this.ready;
    const def = keyDefinition(key);
    if (!def) throw new Error(`不认识的键名：${key}（用 CDP 键名，如 Enter、Tab、ArrowDown、Escape）`);
    const base = { key: def.key, code: def.code, windowsVirtualKeyCode: def.keyCode, nativeVirtualKeyCode: def.keyCode };
    const text = def.text === undefined ? {} : { text: def.text, unmodifiedText: def.text };
    await this.send("Input.dispatchKeyEvent", { type: "keyDown", ...base, ...text });
    await this.send("Input.dispatchKeyEvent", { type: "keyUp", ...base });
  }

  /** A mouse-wheel turn of `dy` pixels (positive is down) over the middle of the viewport. */
  async scroll(dy: number): Promise<void> {
    await this.ready;
    const { result } = await this.send<{ result: { value: { w: number; h: number } } }>("Runtime.evaluate", {
      expression: "({ w: innerWidth, h: innerHeight })",
      returnByValue: true,
    });
    const x = Math.round(result.value.w / 2);
    const y = Math.round(result.value.h / 2);
    await this.send("Input.dispatchMouseEvent", { type: "mouseWheel", x, y, deltaX: 0, deltaY: dy });
  }

  async screenshot(): Promise<{ base64: string; mime: "image/png" }> {
    const image = await this.contents.capturePage();
    return { base64: image.toPNG().toString("base64"), mime: "image/png" };
  }

  console(): ConsoleEntry[] {
    return this.logs.slice();
  }

  private async pointOf(backendNodeId: number): Promise<{ x: number; y: number }> {
    await this.send("DOM.scrollIntoViewIfNeeded", { backendNodeId }).catch(() => undefined);
    let box: { model: { content: number[] } };
    try {
      box = await this.send("DOM.getBoxModel", { backendNodeId });
    } catch {
      throw new Error("这个元素已不在页面上或看不见，请重新 browser_snapshot");
    }
    return quadCenter(box.model.content);
  }
}

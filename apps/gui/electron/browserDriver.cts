// One browser tab driven through the Chrome DevTools Protocol (`webContents.debugger`, protocol
// 1.3). Its parsing and key tables are in browserCdp.cts. One instance per <webview>, made when
// the tab attaches and kept until it is destroyed (electron/browser.cts).

import type { WebContents } from "electron";
import { axSnapshot, backendNodeFor, consoleEntry, keyDefinition, quadCenter, type AxNode, type ConsoleEntry } from "./browserCdp.cjs";

/** Console entries kept per tab; older ones are dropped. */
const CONSOLE_LIMIT = 200;

/** Input types that take typed text as a plain string value. */
const TEXT_INPUT_TYPES = ["text", "search", "url", "tel", "email", "password", "number"];

/**
 * Run in the page on the element. For an input or textarea: focuses it, writes the text at the
 * caret through the element's own value setter (so frameworks see the change), and fires `input`
 * and `change`. Returns "value". For a contenteditable: focuses it and returns "editable" (the
 * text is typed by the caller). Anything else returns "none", and nothing is changed.
 */
const FOCUS_AND_SET = `function (text) {
  const isText = this instanceof HTMLTextAreaElement || (this instanceof HTMLInputElement && ${JSON.stringify(TEXT_INPUT_TYPES)}.includes(this.type));
  if (isText) {
    if (this.disabled || this.readOnly) return "none";
    this.focus();
    const proto = this instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    const setValue = Object.getOwnPropertyDescriptor(proto, "value").set;
    const old = this.value;
    const hasCaret = this.selectionStart !== null && this.selectionEnd !== null;
    const s = hasCaret ? this.selectionStart : old.length;
    const e = hasCaret ? this.selectionEnd : old.length;
    setValue.call(this, old.slice(0, s) + text + old.slice(e));
    if (hasCaret) {
      try { this.setSelectionRange(s + text.length, s + text.length); } catch (_) { /* type without a caret */ }
    }
    this.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText", data: text }));
    this.dispatchEvent(new Event("change", { bubbles: true }));
    return "value";
  }
  if (this.isContentEditable) {
    this.focus();
    return "editable";
  }
  return "none";
}`;

/** Run in the page on the element after typing: whether its text now holds `text`. */
const CONTAINS_TEXT = `function (text) {
  const value = typeof this.value === "string" ? this.value : (this.textContent ?? "");
  return value.includes(text);
}`;

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

  /**
   * Types `text` into the element with `ref` and presses Enter when `submit`. A plain input or
   * textarea gets its value set directly (the native setter, then `input` and `change`), so no key
   * event is sent to the window. A contenteditable gets the text as input, which needs the page to
   * hold the keyboard focus (checked first). Either way the text is read back afterwards.
   */
  async type(ref: number, text: string, submit: boolean): Promise<void> {
    await this.ready;
    const backendNodeId = backendNodeFor(this.refs, ref);
    let resolved: { object: { objectId?: string } };
    try {
      resolved = await this.send("DOM.resolveNode", { backendNodeId });
    } catch {
      throw new Error(`ref=${ref} 不能输入文字（不是输入框），请重新 browser_snapshot`);
    }
    const objectId = resolved.object.objectId;
    if (!objectId) throw new Error(`ref=${ref} 不能输入文字（不是输入框），请重新 browser_snapshot`);
    try {
      const kind = await this.callOn<string>(objectId, FOCUS_AND_SET, [text]);
      if (kind === "none") throw new Error(`ref=${ref} 不能输入文字（不是输入框），请重新 browser_snapshot`);
      if (kind === "editable") {
        await this.requireKeyboardFocus();
        await this.send("Input.insertText", { text });
      }
      const written = await this.callOn<boolean>(objectId, CONTAINS_TEXT, [text]);
      if (!written) throw new Error(`ref=${ref} 的文字没有写进输入框（页面可能拒绝了这段文字，如长度或格式限制）`);
    } finally {
      await this.send("Runtime.releaseObject", { objectId }).catch(() => undefined);
    }
    if (submit) await this.press("Enter");
  }

  /** One key, pressed and released. `key` is a CDP key name such as `Enter` or `ArrowDown`. */
  async press(key: string): Promise<void> {
    await this.ready;
    const def = keyDefinition(key);
    if (!def) throw new Error(`不认识的键名：${key}（用 CDP 键名，如 Enter、Tab、ArrowDown、Escape）`);
    await this.requireKeyboardFocus();
    const base = { key: def.key, code: def.code, windowsVirtualKeyCode: def.keyCode, nativeVirtualKeyCode: def.keyCode };
    const text = def.text === undefined ? {} : { text: def.text, unmodifiedText: def.text };
    await this.send("Input.dispatchKeyEvent", { type: "keyDown", ...base, ...text });
    await this.send("Input.dispatchKeyEvent", { type: "keyUp", ...base });
  }

  /**
   * Before any key is sent: the guest takes the window's focus, and the page must report it has
   * the keyboard. Otherwise the key would go to whatever widget of the app has focus, so it is
   * refused instead.
   */
  private async requireKeyboardFocus(): Promise<void> {
    this.contents.focus();
    const { result } = await this.send<{ result: { value: unknown } }>("Runtime.evaluate", {
      expression: "document.hasFocus()",
      returnByValue: true,
    });
    if (result.value !== true) throw new Error("网页没有拿到键盘焦点，已取消按键");
  }

  /** Calls a function (source in `declaration`) with `this` = the object, returns its value. */
  private async callOn<T>(objectId: string, declaration: string, args: unknown[]): Promise<T> {
    const { result } = await this.send<{ result: { value: T } }>("Runtime.callFunctionOn", {
      objectId,
      functionDeclaration: declaration,
      arguments: args.map((value) => ({ value })),
      returnByValue: true,
    });
    return result.value;
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

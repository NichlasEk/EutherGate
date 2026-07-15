declare module "@novnc/novnc" {
  export default class RFB extends EventTarget {
    constructor(target: HTMLElement, url: string, options?: { shared?: boolean; credentials?: Record<string, string> });
    background: string;
    compressionLevel: number;
    qualityLevel: number;
    resizeSession: boolean;
    scaleViewport: boolean;
    viewOnly: boolean;
    disconnect(): void;
    focus(): void;
    sendKey(keysym: number, code: string, down?: boolean): void;
  }
}

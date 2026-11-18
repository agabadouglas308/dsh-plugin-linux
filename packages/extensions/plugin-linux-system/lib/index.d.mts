/**
 * Cordis plugin: linux-system
 *
 * Native Linux system capabilities for DeepSeek Harness.
 * Registers 7 tools: systemd (status, action), journalctl, clipboard (get, set),
 * containers, and desktop notifications.
 */
import { Context } from "@deepseek-ai/cordis";
export declare const name = "plugin-linux-system";
export declare const inject: string[];
export declare function apply(ctx: Context): void;
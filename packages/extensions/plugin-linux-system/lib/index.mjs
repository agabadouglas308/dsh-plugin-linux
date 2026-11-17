import { defineTool } from "@deepseek-ai/dsh-tools";
import { execFile, spawn } from "node:child_process";
import { promisify } from "node:util";
import net from "node:net";
//#region src/index.ts
const execFileP = promisify(execFile);
const name = "plugin-linux-system";
const inject = ["tools"];
async function runCmd(cmd, args, timeoutMs = 15e3) {
	const { stdout, stderr } = await execFileP(cmd, args, {
		timeout: timeoutMs,
		maxBuffer: 8 * 1024 * 1024
	});
	return {
		stdout: String(stdout),
		stderr: String(stderr)
	};
}
function spawnWithStdin(cmd, args, input) {
	return new Promise((resolve, reject) => {
		const child = spawn(cmd, args, { stdio: [
			"pipe",
			"ignore",
			"pipe"
		] });
		const errChunks = [];
		child.stderr?.on("data", (d) => errChunks.push(d));
		child.on("error", reject);
		child.on("close", (code) => {
			if (code === 0) {
				resolve();
				return;
			}
			reject(/* @__PURE__ */ new Error(`${cmd} exited ${code}: ${Buffer.concat(errChunks).toString("utf8").trim()}`));
		});
		child.stdin?.end(input);
	});
}
function scopeArgs(scope) {
	return scope === "user" ? ["--user"] : [];
}
async function readClipboard() {
	const errors = [];
	if (process.env.WAYLAND_DISPLAY) try {
		return (await runCmd("wl-paste", [
			"--no-newline",
			"--type",
			"text/plain"
		], 3e3)).stdout;
	} catch (e) {
		errors.push(String(e));
	}
	try {
		return (await runCmd("xclip", [
			"-selection",
			"clipboard",
			"-o"
		], 3e3)).stdout;
	} catch (e) {
		errors.push(String(e));
	}
	try {
		return (await runCmd("xsel", ["--clipboard", "--output"], 3e3)).stdout;
	} catch (e) {
		errors.push(String(e));
	}
	throw new Error("No clipboard reader available (wl-paste / xclip / xsel). Errors:\n" + errors.join("\n"));
}
async function writeClipboard(text) {
	const errors = [];
	if (process.env.WAYLAND_DISPLAY) try {
		await spawnWithStdin("wl-copy", ["--type", "text/plain"], text);
		return;
	} catch (e) {
		errors.push(String(e));
	}
	try {
		await spawnWithStdin("xclip", ["-selection", "clipboard"], text);
		return;
	} catch (e) {
		errors.push(String(e));
	}
	try {
		await spawnWithStdin("xsel", ["--clipboard", "--input"], text);
		return;
	} catch (e) {
		errors.push(String(e));
	}
	throw new Error("No clipboard writer available (wl-copy / xclip / xsel). Errors:\n" + errors.join("\n"));
}
async function sendNotification(args) {
	const payload = JSON.stringify({
		summary: args.summary,
		body: args.body ?? "",
		urgency: args.urgency ?? "normal"
	});
	const sockPath = process.env.DSH_NOTIFY_SOCK;
	if (sockPath) try {
		await new Promise((resolve, reject) => {
			const c = net.createConnection(sockPath);
			c.on("connect", () => {
				c.write(payload + "\n");
				c.end();
			});
			c.on("end", () => resolve());
			c.on("error", (e) => reject(e));
		});
		return {
			ok: true,
			via: "socket"
		};
	} catch {}
	await execFileP("notify-send", [
		"-a",
		"DeepSeek Harness",
		"-u",
		args.urgency === "critical" ? "critical" : args.urgency === "low" ? "low" : "normal",
		args.summary,
		args.body ?? ""
	], { timeout: 5e3 });
	return {
		ok: true,
		via: "notify-send"
	};
}
function apply(ctx) {
	ctx.tools.register(defineTool({
		name: "linux_systemd_status",
		description: "Return structured status of a systemd unit: LoadState, ActiveState, SubState, UnitFileState, MainPID, Description, FragmentPath, ExecMainStartTimestamp. Default scope is system; pass scope=\"user\" for --user units.",
		parameters: {
			unit: {
				type: "string",
				required: true,
				description: "Unit name, e.g. \"nginx.service\" or \"ssh.socket\"."
			},
			scope: {
				type: "string",
				enum: ["system", "user"],
				description: "\"system\" (default) or \"user\"."
			}
		},
		output: {
			schema: { type: "json" },
			render: (_args, value) => [{
				type: "text",
				text: JSON.stringify(value, null, 2)
			}]
		},
		isConcurrencySafe: () => true,
		async execute(args) {
			const scope = args.scope ?? "system";
			const { stdout } = await runCmd("systemctl", [
				...scopeArgs(scope),
				"show",
				args.unit,
				"--property=LoadState,ActiveState,SubState,UnitFileState,MainPID,Description,FragmentPath,ExecMainStartTimestamp",
				"--no-pager"
			]);
			const info = {};
			for (const line of stdout.trim().split("\n")) {
				const eq = line.indexOf("=");
				if (eq > 0) info[line.slice(0, eq)] = line.slice(eq + 1);
			}
			return {
				unit: args.unit,
				scope,
				properties: info
			};
		}
	}));
	ctx.tools.register(defineTool({
		name: "linux_systemd_action",
		description: "Start, stop, restart, enable, or disable a systemd unit. Mutating — runs serially.",
		parameters: {
			unit: {
				type: "string",
				required: true,
				description: "Unit name."
			},
			action: {
				type: "string",
				required: true,
				enum: [
					"start",
					"stop",
					"restart",
					"enable",
					"disable"
				],
				description: "Action to perform."
			},
			scope: {
				type: "string",
				enum: ["system", "user"],
				description: "\"system\" (default) or \"user\"."
			}
		},
		output: {
			schema: { type: "json" },
			render: (_args, value) => [{
				type: "text",
				text: JSON.stringify(value, null, 2)
			}]
		},
		isConcurrencySafe: () => false,
		async execute(args) {
			const scope = args.scope ?? "system";
			const { stdout, stderr } = await runCmd("systemctl", [
				...scopeArgs(scope),
				args.action,
				args.unit
			]);
			return {
				unit: args.unit,
				action: args.action,
				scope,
				stdout: stdout.trim(),
				stderr: stderr.trim()
			};
		}
	}));
	ctx.tools.register(defineTool({
		name: "linux_journal",
		description: "Read structured journalctl entries. Optionally filter by unit and time window. Returns one JSON object per journal entry.",
		parameters: {
			unit: {
				type: "string",
				description: "Filter by systemd unit (optional)."
			},
			lines: {
				type: "number",
				description: "Max entries to return. Default 100."
			},
			since: {
				type: "string",
				description: "Time window, e.g. \"10 min ago\" or \"2024-01-01 12:00\"."
			}
		},
		output: {
			schema: { type: "json" },
			render: (_args, value) => [{
				type: "text",
				text: JSON.stringify(value, null, 2)
			}]
		},
		isConcurrencySafe: () => true,
		async execute(args) {
			const argv = [
				"--no-pager",
				"--output=json",
				"-n",
				String(args.lines ?? 100)
			];
			if (args.unit) argv.push("-u", args.unit);
			if (args.since) argv.push("--since", args.since);
			const { stdout } = await runCmd("journalctl", argv, 2e4);
			const entries = [];
			for (const line of stdout.trim().split("\n")) {
				if (!line) continue;
				try {
					entries.push(JSON.parse(line));
				} catch {
					entries.push({ MESSAGE: line });
				}
			}
			return {
				unit: args.unit ?? null,
				count: entries.length,
				entries
			};
		}
	}));
	ctx.tools.register(defineTool({
		name: "linux_clipboard_get",
		description: "Read plain text from the system clipboard. Uses Wayland (wl-paste) if WAYLAND_DISPLAY is set, otherwise X11 (xclip / xsel).",
		parameters: {},
		output: {
			schema: { type: "json" },
			render: (_args, value) => [{
				type: "text",
				text: JSON.stringify(value, null, 2)
			}]
		},
		isConcurrencySafe: () => true,
		async execute() {
			const text = await readClipboard();
			return {
				text,
				length: text.length
			};
		}
	}));
	ctx.tools.register(defineTool({
		name: "linux_clipboard_set",
		description: "Write plain text to the system clipboard. Same backend selection as linux_clipboard_get.",
		parameters: { text: {
			type: "string",
			required: true,
			description: "Text to place on the clipboard."
		} },
		output: {
			schema: { type: "json" },
			render: (_args, value) => [{
				type: "text",
				text: JSON.stringify(value, null, 2)
			}]
		},
		isConcurrencySafe: () => false,
		async execute(args) {
			await writeClipboard(args.text);
			return {
				ok: true,
				length: args.text.length
			};
		}
	}));
	ctx.tools.register(defineTool({
		name: "linux_containers",
		description: "List running containers via podman (preferred) or docker. Returns the runtime used and the parsed container list.",
		parameters: {},
		output: {
			schema: { type: "json" },
			render: (_args, value) => [{
				type: "text",
				text: JSON.stringify(value, null, 2)
			}]
		},
		isConcurrencySafe: () => true,
		async execute() {
			const errors = [];
			for (const runtime of ["podman", "docker"]) try {
				const { stdout } = await runCmd(runtime, [
					"ps",
					"--format",
					"json"
				], 1e4);
				let containers;
				try {
					containers = JSON.parse(stdout);
				} catch {
					containers = stdout.trim();
				}
				return {
					runtime,
					containers
				};
			} catch (e) {
				errors.push(`${runtime}: ${String(e)}`);
			}
			throw new Error("neither podman nor docker is available:\n" + errors.join("\n"));
		}
	}));
	ctx.tools.register(defineTool({
		name: "linux_notify",
		description: "Send a native desktop notification (D-Bus). Uses the wrapper socket when DSH_NOTIFY_SOCK is set, else falls back to notify-send. Urgency: low / normal / critical.",
		parameters: {
			summary: {
				type: "string",
				required: true,
				description: "Notification title."
			},
			body: {
				type: "string",
				description: "Notification body text."
			},
			urgency: {
				type: "string",
				enum: [
					"low",
					"normal",
					"critical"
				],
				description: "Notification urgency (default: normal)."
			}
		},
		output: {
			schema: { type: "json" },
			render: (_args, value) => [{
				type: "text",
				text: JSON.stringify(value, null, 2)
			}]
		},
		isConcurrencySafe: () => true,
		async execute(args) {
			return await sendNotification(args);
		}
	}));
}
//#endregion
export { apply, inject, name };

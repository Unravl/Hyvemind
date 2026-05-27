import type { ExtensionAPI, ProviderModelConfig } from "@mariozechner/pi-coding-agent";

interface HyvemindProviderManifestEntry {
	id: string;
	displayName?: string;
	baseUrl?: string;
	endpointEnvVar?: string;
	apiKeyEnvVar?: string;
}

interface HyvemindProviderManifest {
	providers?: HyvemindProviderManifestEntry[];
}

interface ModelEndpointResponse {
	data?: Array<{
		id?: string;
		name?: string;
		context_length?: number;
		context_window?: number;
		max_context_length?: number;
		max_model_len?: number;
		max_output_tokens?: number;
		max_tokens?: number;
		max_completion_tokens?: number;
		top_provider?: { max_completion_tokens?: number };
	}>;
}

const DEFAULT_CONTEXT_WINDOW = 128_000;
const DEFAULT_MAX_TOKENS = 16_384;
const MODEL_FETCH_TIMEOUT_MS = 2_500;

type CapabilityPatch = Pick<ProviderModelConfig, "reasoning"> & {
	thinkingLevelMap?: ProviderModelConfig["thinkingLevelMap"];
	compat?: Partial<NonNullable<ProviderModelConfig["compat"]>>;
};

// Vision-capable models keyed by `${providerId}/${modelId}`. The default
// `input` for unknown models is text-only — over-claiming `image` for a
// text-only provider causes Pi to forward image content blocks the
// upstream API rejects, with no useful error surfaced to the user.
const VISION_MODELS = new Set<string>([
	"crof/crof-gpt-4o",
	"crof/crof-gpt-4o-mini",
	"openai/gpt-4o",
	"openai/gpt-4o-mini",
	"openai/gpt-4.1",
	"openai/gpt-5",
	"openai/gpt-5-mini",
]);

function isVisionCapable(providerId: string, modelId: string): boolean {
	return VISION_MODELS.has(`${providerId}/${modelId}`);
}

// Per-(provider, modelId) reasoning capability overrides. Without these,
// `normalizeModel` defaults `reasoning: false`, which causes Pi's
// `clampThinkingLevel` to collapse any requested level to "off" — silently
// disabling `--thinking high` for reasoning-capable models. Shapes mirror
// Pi's own packages/ai/src/models.generated.ts where possible.
const REASONING_CAPABILITIES: Record<string, Record<string, CapabilityPatch>> = {
	deepseek: {
		"deepseek-reasoner": {
			reasoning: true,
			thinkingLevelMap: { minimal: null, low: null, medium: null, high: "high", xhigh: "max" },
			compat: { thinkingFormat: "deepseek", supportsReasoningEffort: true },
		},
		"deepseek-v4-pro": {
			reasoning: true,
			thinkingLevelMap: { minimal: null, low: null, medium: null, high: "high", xhigh: "max" },
			compat: { thinkingFormat: "deepseek", supportsReasoningEffort: true },
		},
	},
	glm: {
		"glm-4.6": {
			reasoning: true,
			thinkingLevelMap: { minimal: null, low: null, medium: null, high: "high" },
			compat: { thinkingFormat: "zai", supportsReasoningEffort: true },
		},
	},
	openai: {
		"gpt-5": {
			reasoning: true,
			thinkingLevelMap: { off: null },
			compat: { supportsReasoningEffort: true, maxTokensField: "max_completion_tokens", supportsDeveloperRole: true },
		},
		"gpt-5-mini": {
			reasoning: true,
			thinkingLevelMap: { off: null },
			compat: { supportsReasoningEffort: true, maxTokensField: "max_completion_tokens", supportsDeveloperRole: true },
		},
		o1: {
			reasoning: true,
			thinkingLevelMap: { off: null },
			compat: { supportsReasoningEffort: true, maxTokensField: "max_completion_tokens", supportsDeveloperRole: true },
		},
		o3: {
			reasoning: true,
			thinkingLevelMap: { off: null },
			compat: { supportsReasoningEffort: true, maxTokensField: "max_completion_tokens", supportsDeveloperRole: true },
		},
		"o4-mini": {
			reasoning: true,
			thinkingLevelMap: { off: null },
			compat: { supportsReasoningEffort: true, maxTokensField: "max_completion_tokens", supportsDeveloperRole: true },
		},
	},
};

const STATIC_MODELS: Record<string, Array<Partial<ProviderModelConfig> & { id: string; name?: string }>> = {
	crof: [
		{ id: "mimo-v2.5-pro-precision", name: "MiMo v2.5 Pro Precision", contextWindow: 128_000, maxTokens: 16_384 },
		{ id: "crof-gpt-4o", name: "Crof GPT-4o", contextWindow: 128_000, maxTokens: 16_384 },
		{ id: "crof-gpt-4o-mini", name: "Crof GPT-4o Mini", contextWindow: 128_000, maxTokens: 16_384 },
	],
	deepseek: [
		{ id: "deepseek-chat", name: "DeepSeek Chat", contextWindow: 128_000, maxTokens: 16_384 },
		{ id: "deepseek-reasoner", name: "DeepSeek Reasoner", contextWindow: 128_000, maxTokens: 16_384 },
		{ id: "deepseek-coder", name: "DeepSeek Coder", contextWindow: 128_000, maxTokens: 16_384 },
	],
	glm: [
		{ id: "glm-4-plus", name: "GLM-4 Plus", contextWindow: 128_000, maxTokens: 16_384 },
		{ id: "glm-4.6", name: "GLM-4.6", contextWindow: 128_000, maxTokens: 16_384 },
	],
	mistral: [
		{ id: "mistral-large-latest", name: "Mistral Large", contextWindow: 128_000, maxTokens: 16_384 },
		{ id: "mistral-small-latest", name: "Mistral Small", contextWindow: 128_000, maxTokens: 16_384 },
	],
	groq: [
		{ id: "llama-3.3-70b-versatile", name: "Llama 3.3 70B Versatile", contextWindow: 128_000, maxTokens: 16_384 },
		{ id: "llama-3.1-8b-instant", name: "Llama 3.1 8B Instant", contextWindow: 128_000, maxTokens: 8_192 },
	],
	kimi: [
		{ id: "kimi-k2-instruct", name: "Kimi K2 Instruct", contextWindow: 256_000, maxTokens: 16_384 },
	],
	openai: [
		{ id: "gpt-4o", name: "GPT-4o", contextWindow: 128_000, maxTokens: 16_384 },
		{ id: "gpt-4o-mini", name: "GPT-4o Mini", contextWindow: 128_000, maxTokens: 16_384 },
		{ id: "gpt-4.1", name: "GPT-4.1", contextWindow: 1_000_000, maxTokens: 16_384 },
		{ id: "gpt-5", name: "GPT-5", contextWindow: 272_000, maxTokens: 16_384 },
		{ id: "gpt-5-mini", name: "GPT-5 Mini", contextWindow: 272_000, maxTokens: 16_384 },
	],
};

function parseManifest(): HyvemindProviderManifest {
	const raw = process.env.HYVEMIND_PI_PROVIDERS_JSON;
	if (!raw) return {};
	try {
		return JSON.parse(raw) as HyvemindProviderManifest;
	} catch (error) {
		console.error(`[hyvemind-providers] failed to parse HYVEMIND_PI_PROVIDERS_JSON: ${error instanceof Error ? error.message : String(error)}`);
		return {};
	}
}

function capabilityFor(providerId: string, modelId: string): CapabilityPatch | undefined {
	return REASONING_CAPABILITIES[providerId]?.[modelId];
}

function normalizeModel(providerId: string, entry: Partial<ProviderModelConfig> & { id: string; name?: string }): ProviderModelConfig {
	const patch = entry.reasoning === undefined ? capabilityFor(providerId, entry.id) : undefined;
	const reasoning = entry.reasoning ?? patch?.reasoning ?? false;
	const thinkingLevelMap = entry.thinkingLevelMap ?? patch?.thinkingLevelMap;
	const baseCompat = {
		supportsDeveloperRole: false,
		supportsReasoningEffort: false,
		supportsUsageInStreaming: false,
		maxTokensField: "max_tokens" as const,
	};
	const compat = entry.compat ?? { ...baseCompat, ...(patch?.compat ?? {}) };
	const defaultInput: ProviderModelConfig["input"] = isVisionCapable(providerId, entry.id) ? ["text", "image"] : ["text"];
	return {
		id: entry.id,
		name: entry.name ?? entry.id,
		api: "openai-completions",
		reasoning,
		input: entry.input ?? defaultInput,
		cost: entry.cost ?? { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
		contextWindow: entry.contextWindow ?? DEFAULT_CONTEXT_WINDOW,
		maxTokens: entry.maxTokens ?? DEFAULT_MAX_TOKENS,
		...(thinkingLevelMap ? { thinkingLevelMap } : {}),
		compat,
	};
}

function modelsFromStatic(providerId: string): ProviderModelConfig[] {
	return (STATIC_MODELS[providerId] ?? []).map((entry) => normalizeModel(providerId, entry));
}

function modelsFromResponse(providerId: string, body: ModelEndpointResponse): ProviderModelConfig[] {
	return (body.data ?? [])
		.filter((m): m is NonNullable<ModelEndpointResponse["data"]>[number] & { id: string } => typeof m.id === "string" && m.id.length > 0)
		.map((m) => normalizeModel(providerId, {
			id: m.id,
			name: m.name ?? m.id,
			contextWindow: m.context_length ?? m.context_window ?? m.max_context_length ?? m.max_model_len ?? DEFAULT_CONTEXT_WINDOW,
			maxTokens: m.max_output_tokens ?? m.max_tokens ?? m.max_completion_tokens ?? m.top_provider?.max_completion_tokens ?? DEFAULT_MAX_TOKENS,
		}));
}

async function fetchModels(providerId: string, baseUrl: string, apiKeyEnvVar?: string): Promise<ProviderModelConfig[]> {
	const controller = new AbortController();
	const timeout = setTimeout(() => controller.abort(), MODEL_FETCH_TIMEOUT_MS);
	try {
		const headers: Record<string, string> = { accept: "application/json" };
		const apiKey = apiKeyEnvVar ? process.env[apiKeyEnvVar] : undefined;
		if (apiKey) headers.authorization = `Bearer ${apiKey}`;
		const response = await fetch(`${baseUrl.replace(/\/+$/, "")}/models`, {
			method: "GET",
			headers,
			signal: controller.signal,
		});
		if (!response.ok) return [];
		return modelsFromResponse(providerId, (await response.json()) as ModelEndpointResponse);
	} catch {
		return [];
	} finally {
		clearTimeout(timeout);
	}
}

function mergeModels(primary: ProviderModelConfig[], fallback: ProviderModelConfig[]): ProviderModelConfig[] {
	const byId = new Map<string, ProviderModelConfig>();
	for (const m of fallback) byId.set(m.id, m);
	for (const m of primary) byId.set(m.id, { ...byId.get(m.id), ...m });
	return [...byId.values()];
}

export default async function (pi: ExtensionAPI) {
	const manifest = parseManifest();
	await Promise.all((manifest.providers ?? []).map(async (provider) => {
		if (!provider.id) return;
		const baseUrl = (provider.endpointEnvVar ? process.env[provider.endpointEnvVar] : undefined) || provider.baseUrl;
		if (!baseUrl) return;

		const hasApiKey = provider.apiKeyEnvVar ? Boolean(process.env[provider.apiKeyEnvVar]) : false;
		const isLocal = /localhost|127\.0\.0\.1|\[::1\]/.test(baseUrl);
		if (!hasApiKey && !isLocal) return;

		const fallback = modelsFromStatic(provider.id);
		const fetched = await fetchModels(provider.id, baseUrl, provider.apiKeyEnvVar);
		const models = mergeModels(fetched, fallback);
		if (models.length === 0) return;

		pi.registerProvider(provider.id, {
			name: provider.displayName ?? provider.id,
			baseUrl,
			apiKey: hasApiKey ? provider.apiKeyEnvVar : undefined,
			api: "openai-completions",
			authHeader: hasApiKey,
			models,
		});
		console.error(`[hyvemind-providers] registered ${provider.id} (${models.length} models)`);
	}));
}

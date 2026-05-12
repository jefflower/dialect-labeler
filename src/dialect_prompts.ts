/**
 * Per-dialect prompt bundles. The Ollama polish prompt (`llmPrompt`) for
 * Changsha is intentionally an empty string — the Rust backend falls back
 * to the built-in `default_llm_prompt.txt` (which IS the long-form
 * Changsha rules) when llmPrompt is blank. Keeps a single source of truth
 * for that one.
 *
 * Taiwanese is shipped as a full inline string because the backend has
 * no built-in for it. It's authored as a starter — refine against real
 * audio once a few batches are labelled.
 */
import type { DialectProfile } from "./types";

const TAIWAN_LLM_PROMPT = `你是一个台湾方言（台湾国语 / 台湾闽南语）的资深“记音字”转写 + 副语言标注专家。任务：把 Whisper 普通话初稿改写为台湾方言**记音字**，并按以下规范积极标注**副语言事件**与**情感**。

## 一、转写忠实度（**最高优先级**）

### 1.1 所听即所写
- **使用中文简体**忠实地按音频实际念出来的字转写。
- **数字一律用中文写**（按 TN 规范），不用阿拉伯数字、不用百分号：
  - 错：\`20%\`、\`12:30\`、\`100\`
  - 对：\`百分之二十\`、\`十二点三十\`、\`一百\`
- **重复字必须保留**：音频实际念两次就转两次。
  - "我一直一直在等" → \`我一直一直在等。\`

### 1.2 记音字优先（针对 "发音人" 角色）
- **优先选用发音接近的汉字**，而非语义对应的字。
- 台湾国语口语 / 闽南语借词的记音字直接照写。

### 1.3 角色分流
- **陪聊（普通话提问者）**：保持普通话原貌，仅修正同音错别字、漏字、断句。
- **发音人（说台湾方言）**：严格按发音改为方言记音字。

### 1.4 台湾话常见对照
什么→蝦米 / 蛤；不好意思→歹勢；干嘛→衝啥小；对啊→係啊 / 嘿啊；不会啦→袂啦；这样→醬子；那样→揩款；非常 / 很→足、有夠、超；好棒→好棒棒；语末助词：啦、喔、欸、捏、咧、嘛、吼、捏、啊。英文 / 专有名词原样保留。

## 二、副语言事件（**严格按以下两类语法**）

### 2.1 离散事件标签 \`[xxx]\`（方括号，无内容，单点出现）
共 8 种：

| 标签 | 含义 |
|---|---|
| \`[laugh]\` | 笑声 |
| \`[breath]\` | 呼吸声 |
| \`[cough]\` | 咳嗽 |
| \`[clucking]\` | 咯咯笑 |
| \`[hissing]\` | 嘘声 |
| \`[sigh]\` | 叹气 |
| \`[lipsmack]\` | 舔唇/咂嘴 |
| \`[swallowing]\` | 吞口水声 |

### 2.2 成对包裹标签 \`<xxx>...</xxx>\`（XML 形式，包文本）
仅 2 种：

| 标签 | 含义 |
|---|---|
| \`<laugh>...</laugh>\` | 笑着说 |
| \`<strong>...</strong>\` | 重读（1-3 字） |

### 2.3 副语言关键约束
1. 多个副语言标签不要紧挨同时出现
2. 笑声后紧接呼吸声 → 统一用 \`[laugh]\` 或 \`<laugh>...</laugh>\`
3. \`[xxx]\` 与 \`<laugh>...</laugh>\` 互斥
4. 不要发明 10 种之外的标签，不要 markdown，不要 emoji

## 三、情感（句级别，**必选 1 个**）

枚举：**中立、开心、愤怒、悲伤、惊讶、恐惧、厌恶**

判据：
- 句末 "蛤?"、"係哦?"、"真的喔?"、"齁?" → **惊讶**
- "哈哈"、调侃、活泼语气 → **开心**
- 抱怨、命令、急促 → **愤怒**
- 哽咽、低沉、叹气、慢节奏 → **悲伤**
- 受惊、紧张、颤抖 → **恐惧**
- 嫌弃、反感、鄙夷 → **厌恶**
- 平静叙述、无明显情绪 → **中立**

约束：
- 情感标签不涉及演绎身份（"严肃"、"活泼"等性格词不算情感）
- 一个单句的情感保持稳定
- 文案演绎源数据若已有情绪描述，**适度归纳**到这 7 类之一

## 四、输出格式（**严格 JSON，一行内**）

\`\`\`
{"text": "<带内联标签的台湾方言记音字稿>", "emotion": "<情感枚举>", "tags": ["<可选段级标签>"]}
\`\`\`

字段：
- \`text\`：方言记音字稿（含 \`[xxx]\` 和 \`<xxx>...</xxx>\`，所有数字中文，重复字保留）
- \`emotion\`：从 7 个情感中选 1 个
- \`tags\`：可选数组，整段最显著的副语言标签名（laugh/breath/cough/...），无则 \`[]\`

不要 markdown 代码块，不要 \`\`\`json\`\`\` 前缀，不要解释。

## 五、示例对照

**例 1**：陪聊，含数字
- Whisper：\`这个产品销量增长了20%达到100台\`
- 输出：\`{"text": "这个产品销量增长了百分之二十，达到一百台。", "emotion": "中立", "tags": []}\`

**例 2**：发音人，含口语重复
- Whisper：\`你怎么这么这么夸张啦\`
- 输出：\`{"text": "你怎么这么这么夸张啦。", "emotion": "中立", "tags": []}\`

**例 3**：发音人
- Whisper：\`这个我吃过欸真的好吃\`
- 输出：\`{"text": "这个我吃过欸，<strong>真的</strong>好吃啦。", "emotion": "中立", "tags": []}\`

**例 4**：发音人，边笑边说
- Whisper：\`哈哈干嘛啦你超烦的啦\`
- 输出：\`{"text": "<laugh>衝啥小啦</laugh>，你超烦的啦。", "emotion": "开心", "tags": ["laugh"]}\`

**例 5**：发音人，独立笑声
- Whisper：\`哈哈不会吧真的假的\`
- 输出：\`{"text": "[laugh]袂啦，<strong>真的</strong>假的啦？", "emotion": "开心", "tags": ["laugh"]}\`

如初稿已是合理记音字稿，\`text\` 原样返回，仍要补 \`emotion\` 和 \`tags\`。
`;

export const builtinDialectProfiles: DialectProfile[] = [
  {
    id: "changsha",
    name: "长沙话",
    builtin: true,
    whisperInitialPrompt:
      "以下是中文方言（长沙话）口语转写，请用汉字记录听到的字音，不要翻译。",
    // Empty → Rust backend falls back to default_llm_prompt.txt (the
    // long-form Changsha rules baked into the binary).
    llmPrompt: "",
    systemPrompt: "长沙本地人，女性，25岁左右，声音娇柔，声音清亮",
  },
  {
    id: "taiwan",
    name: "台湾话",
    builtin: true,
    whisperInitialPrompt:
      "以下是中文方言（台湾国语 / 台湾闽南语）口语转写，请用汉字记录听到的字音，不要翻译。",
    llmPrompt: TAIWAN_LLM_PROMPT,
    systemPrompt: "台湾本地人，声音自然清亮，口语化",
  },
];

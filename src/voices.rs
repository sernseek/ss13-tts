use std::{collections::HashSet, path::Path};

use serde::{Deserialize, Serialize};

use crate::provider::Backend;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Gender {
    Female,
    Male,
}

/// A voice the game can select. `display` is the stable identifier stored in character
/// preferences and must keep its "Man"/"Woman" suffix: the game infers gender from it.
#[derive(Clone, Debug, Serialize)]
pub struct Voice {
    #[serde(rename = "id")]
    pub display: String,
    #[serde(skip)]
    pub api: String,
    pub label: String,
    pub gender: Gender,
    pub description: String,
    /// Forced `language_hints` entry for voices that can only speak one language.
    #[serde(skip)]
    pub language: Option<&'static str>,
    /// Whether the game may hand this voice out at random: adult voices that speak the
    /// server's language. Child, English-only and custom voices must be chosen on purpose.
    pub random: bool,
    /// Whether synthesized audio must carry the provider's AIGC watermark.
    #[serde(skip)]
    pub aigc: bool,
}

struct Builtin {
    display: &'static str,
    api: &'static str,
    label: &'static str,
    gender: Gender,
    description: &'static str,
    language: Option<&'static str>,
}

const fn voice(
    display: &'static str,
    api: &'static str,
    label: &'static str,
    gender: Gender,
    description: &'static str,
) -> Builtin {
    Builtin {
        display,
        api,
        label,
        gender,
        description,
        language: None,
    }
}

const fn english(
    display: &'static str,
    api: &'static str,
    gender: Gender,
    description: &'static str,
) -> Builtin {
    Builtin {
        display,
        api,
        label: display,
        gender,
        description,
        language: Some("en"),
    }
}

use Gender::{Female as F, Male as M};

/// System voices of qwen-audio-3.1-tts-flash. Voice parameters are case-sensitive.
const QWEN_AUDIO_VOICES: &[Builtin] = &[
    // Multilingual voices: Mandarin plus Shanghainese, Cantonese, Northeastern, Chongqing,
    // Shaanxi, Yunnan, Ningbo and Gansu dialects, and eight foreign languages.
    voice(
        "Anhuan Woman",
        "longanhuan_v3.1",
        "龙安欢",
        F,
        "多语种·方言",
    ),
    voice(
        "Lingxin Woman",
        "longanlingxin_v3.1",
        "龙安灵心",
        F,
        "多语种·方言",
    ),
    voice(
        "Fengyue Woman",
        "longanfengyue_v3.1",
        "龙安风悦",
        F,
        "多语种·方言",
    ),
    voice(
        "Xu Nanchuan Man",
        "xunanchuan_v3.1",
        "许南川",
        M,
        "多语种·方言",
    ),
    // Premium Mandarin voices.
    voice(
        "Yu Xiaoyun Woman",
        "yuxiaoyun_v3.1",
        "于小云",
        F,
        "元气、亲切",
    ),
    voice(
        "Qiao Xiaojiao Woman",
        "qiaoxiaojiao_v3.1",
        "乔小娇",
        F,
        "俏丽、可爱",
    ),
    voice(
        "Xia Xiaochen Woman",
        "xiaxiaochen_v3.1",
        "夏小晨",
        F,
        "元气、明亮",
    ),
    voice(
        "An Mingyuan Man",
        "anmingyuan_v3.1",
        "安明远",
        M,
        "清亮、自然",
    ),
    voice(
        "Wen Huaiqing Woman",
        "wenhuaiqing_v3.1",
        "温怀清",
        F,
        "清亮、柔和",
    ),
    voice(
        "An Xiaolan Woman",
        "anxiaolan_v3.1",
        "安小岚",
        F,
        "清甜、纯净",
    ),
    voice(
        "Xie Shurou Woman",
        "xieshurou_v3.1",
        "谢舒柔",
        F,
        "柔和、知性",
    ),
    voice(
        "Bai Qinglan Woman",
        "baiqinglan_v3.1",
        "白清岚",
        F,
        "明亮、清纯",
    ),
    voice(
        "Xu Yuyuan Woman",
        "xuyuyuan_v3.1",
        "许玉远",
        F,
        "知性、成熟",
    ),
    voice(
        "An Ruorou Woman",
        "anruorou_v3.1",
        "安若柔",
        F,
        "气声、知性",
    ),
    voice(
        "Wen Huaizhi Woman",
        "wenhuaizhi_v3.1",
        "闻怀之",
        F,
        "稳重、成熟",
    ),
    voice(
        "Xiao Xingzhi Woman",
        "xiaoxingzhi_v3.1",
        "萧行之",
        F,
        "端庄、贵气",
    ),
    voice(
        "Gu Yunshu Woman",
        "guyunshu_v3.1",
        "顾云舒",
        F,
        "成熟、稳重",
    ),
    voice("Huo Zhuoshi Man", "huozhuoshi_v3.1", "霍拙石", M, "清亮"),
    voice(
        "Ye Qinghe Woman",
        "yeqinghe_v3.1",
        "叶清禾",
        F,
        "亲切、温柔",
    ),
    voice(
        "Yun Huanhuan Woman",
        "yunhuanhuan_v3.1",
        "云欢欢",
        F,
        "高亢、热情",
    ),
    voice(
        "Xu Xiaoqiao Woman",
        "xuxiaoqiao_v3.1",
        "徐小俏",
        F,
        "自然、俏皮",
    ),
    voice(
        "Bai Anran Woman",
        "baianran_v3.1",
        "白安然",
        F,
        "低沉、浑厚",
    ),
    voice(
        "Xu Yanchu Woman",
        "xuyanchu_v3.1",
        "许言初",
        F,
        "沉稳、磁性",
    ),
    voice(
        "Ye Zhiqing Woman",
        "yezhiqing_v3.1",
        "叶知晴",
        F,
        "轻快、自然",
    ),
    voice("Andi Man", "andi_v3.1", "安迪", M, "ABC 口音"),
    voice("An Yuqing Woman", "anyuqing_v3.1", "安语晴", F, "甜妹"),
    // Character voices.
    voice(
        "Yuanfei Woman",
        "longanyuanfei_v3.1",
        "龙安元妃",
        F,
        "高傲妃子音",
    ),
    voice(
        "Lingxi Woman",
        "longanlingxi_v3.1",
        "龙安灵希",
        F,
        "可爱甜美音",
    ),
    voice(
        "Yingtao Woman",
        "longyingtao_v3.1",
        "龙应桃",
        F,
        "温柔淡定女",
    ),
    voice("Anya Woman", "longanya_v3.1", "龙安雅", F, "高雅气质女"),
    voice("Wan Woman", "longwan_v3.1", "龙婉", F, "细腻柔声女"),
    voice("Xing Woman", "longxing_v3.1", "龙星", F, "温婉邻家女"),
    voice("Hua Woman", "longhua_v3.1", "龙华", F, "元气甜美女"),
    voice("Han Man", "longhan_v3.1", "龙寒", M, "温暖痴情男"),
    voice("Anzhi Man", "longanzhi_v3.1", "龙安智", M, "睿智轻熟男"),
    voice("Zhe Man", "longzhe_v3.1", "龙哲", M, "呆板大暖男"),
    voice("Anyang Man", "longanyang_v3.1", "龙安洋", M, "阳光大男孩"),
    voice("Li Bai Man", "libai_v3.1", "李白", M, "古代诗仙男"),
    voice(
        "Stella Woman",
        "loongstella_v3.1",
        "Stella",
        F,
        "飒爽利落女",
    ),
    voice("Yuan Woman", "longyuan_v3.1", "龙媛", F, "温暖治愈女"),
    voice("Miao Woman", "longmiao_v3.1", "龙妙", F, "抑扬顿挫女"),
    voice("Sanshu Man", "longsanshu_v3.1", "龙三叔", M, "沉稳质感男"),
    voice("Anli Woman", "longanli_v3.1", "龙安莉", F, "利落从容女"),
    voice("Anwen Woman", "longanwen_v3.1", "龙安温", F, "优雅知性女"),
    voice("Anlang Man", "longanlang_v3.1", "龙安朗", M, "清爽利落男"),
    voice(
        "Xiaoxia Woman",
        "longxiaoxia_v3.1",
        "龙小夏",
        F,
        "沉稳权威女",
    ),
    voice("Anchong Man", "longanchong_v3.1", "龙安冲", M, "激情推销男"),
    // Child voices. Their IDs deliberately lack "Man"/"Woman" so random adult picks skip them.
    voice(
        "Lidou Boy",
        "longjielidou_v3.1",
        "龙杰力豆",
        M,
        "天真男童音",
    ),
    voice("Huohuo Boy", "longhuohuo_v3.1", "龙火火", M, "顽皮少年音"),
    voice("Niuniu Boy", "longniuniu_v3.1", "龙牛牛", M, "阳光男童声"),
    voice(
        "Shanshan Boy",
        "longshanshan_v3.1",
        "龙闪闪",
        M,
        "戏剧化童声",
    ),
    voice("Ling Girl", "longling_v3.1", "龙铃", F, "稚气呆板童声"),
    voice("Paopao Girl", "longpaopao_v3.1", "龙泡泡", F, "飞天泡泡音"),
    // English-only voices.
    english("Emily British Woman", "Emily_v3.1", F, "英式女声，仅英文"),
    english("Luna British Woman", "Luna_v3.1", F, "英式女声，仅英文"),
    english("Eric British Man", "Eric_v3.1", M, "英式男声，仅英文"),
    english("Luca British Man", "Luca_v3.1", M, "英式男声，仅英文"),
    english("Abby American Woman", "Abby_v3.1", F, "美式女声，仅英文"),
    english("Annie American Woman", "Annie_v3.1", F, "美式女声，仅英文"),
    english("Ava American Woman", "Ava_v3.1", F, "美式女声，仅英文"),
    english("Beth American Woman", "Beth_v3.1", F, "美式女声，仅英文"),
    english("Betty American Woman", "Betty_v3.1", F, "美式女声，仅英文"),
    english("Cally American Woman", "Cally_v3.1", F, "美式女声，仅英文"),
    english("Cindy American Woman", "Cindy_v3.1", F, "美式女声，仅英文"),
    english("Donna American Woman", "Donna_v3.1", F, "美式女声，仅英文"),
    english("Andy American Man", "Andy_v3.1", M, "美式男声，仅英文"),
    english("Brian American Man", "Brian_v3.1", M, "美式男声，仅英文"),
    english("David American Man", "David_v3.1", M, "美式男声，仅英文"),
];

/// Voices saved in character preferences before the qwen-audio migration, mapped to the
/// closest qwen-audio voice so existing characters keep a similar voice.
const QWEN_AUDIO_ALIASES: &[(&str, &str)] = &[
    ("Cherry Woman", "Yu Xiaoyun Woman"),
    ("Serena Woman", "Ye Qinghe Woman"),
    ("Chelsie Woman", "Lingxi Woman"),
    ("Vivian Woman", "Qiao Xiaojiao Woman"),
    ("Maia Woman", "Xie Shurou Woman"),
    ("Bellona Woman", "Yun Huanhuan Woman"),
    ("Ethan Man", "Anyang Man"),
    ("Moon Man", "Han Man"),
    ("Neil Man", "Sanshu Man"),
    ("Vincent Man", "Anzhi Man"),
    ("Arthur Man", "Huo Zhuoshi Man"),
    ("Dylan Man", "Anlang Man"),
    ("Jada Woman", "Lingxin Woman"),
    ("Sunny Woman", "Anhuan Woman"),
    ("Eric Man", "Xu Nanchuan Man"),
    ("Kiki Woman", "Fengyue Woman"),
    ("Rocky Man", "An Mingyuan Man"),
];

/// System voices of the qwen3-tts-flash family.
const QWEN_TTS_VOICES: &[Builtin] = &[
    voice("Cherry Woman", "Cherry", "Cherry", F, "阳光积极"),
    voice("Serena Woman", "Serena", "Serena", F, "温柔"),
    voice("Chelsie Woman", "Chelsie", "Chelsie", F, "二次元"),
    voice("Vivian Woman", "Vivian", "Vivian", F, "可爱"),
    voice("Maia Woman", "Maia", "Maia", F, "知性"),
    voice("Bellona Woman", "Bellona", "Bellona", F, "洪亮"),
    voice("Ethan Man", "Ethan", "Ethan", M, "阳光温暖"),
    voice("Moon Man", "Moon", "Moon", M, "率性"),
    voice("Neil Man", "Neil", "Neil", M, "新闻主播"),
    voice("Vincent Man", "Vincent", "Vincent", M, "烟嗓"),
    voice("Arthur Man", "Arthur", "Arthur", M, "质朴老人"),
    voice("Dylan Man", "Dylan", "Dylan", M, "北京话"),
    voice("Jada Woman", "Jada", "Jada", F, "上海话"),
    voice("Sunny Woman", "Sunny", "Sunny", F, "四川话"),
    voice("Eric Man", "Eric", "Eric", M, "四川话"),
    voice("Kiki Woman", "Kiki", "Kiki", F, "粤语"),
    voice("Rocky Man", "Rocky", "Rocky", M, "粤语"),
];

/// A voice created through voice cloning or voice design, loaded from a JSON file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CustomVoice {
    id: String,
    voice: String,
    label: Option<String>,
    gender: Gender,
    #[serde(default)]
    description: String,
}

pub struct VoiceCatalog {
    voices: Vec<Voice>,
    aliases: Vec<(String, String)>,
}

impl VoiceCatalog {
    pub fn new(backend: Backend, custom_voices: Option<&Path>) -> Result<Self, String> {
        let (builtins, aliases) = match backend {
            Backend::QwenAudio => (QWEN_AUDIO_VOICES, QWEN_AUDIO_ALIASES),
            Backend::QwenTts => (QWEN_TTS_VOICES, &[][..]),
        };
        let mut voices: Vec<Voice> = builtins
            .iter()
            .map(|builtin| Voice {
                display: builtin.display.into(),
                api: builtin.api.into(),
                label: builtin.label.into(),
                gender: builtin.gender,
                description: builtin.description.into(),
                language: builtin.language,
                random: builtin.language.is_none()
                    && (builtin.display.ends_with(" Man") || builtin.display.ends_with(" Woman")),
                aigc: false,
            })
            .collect();
        if let Some(path) = custom_voices {
            voices.extend(load_custom_voices(path)?);
        }

        let mut seen = HashSet::new();
        for voice in &voices {
            if !seen.insert(voice.display.as_str()) {
                return Err(format!("duplicate TTS voice id {:?}", voice.display));
            }
        }
        let aliases = aliases
            .iter()
            .filter(|(old, _)| !seen.contains(old))
            .map(|(old, new)| ((*old).to_owned(), (*new).to_owned()))
            .collect();
        Ok(Self { voices, aliases })
    }

    pub fn voices(&self) -> &[Voice] {
        &self.voices
    }

    pub fn aliases(&self) -> impl Iterator<Item = (&str, &str)> {
        self.aliases
            .iter()
            .map(|(old, new)| (old.as_str(), new.as_str()))
    }

    /// Resolves a voice ID, accepting retired IDs that still sit in saved preferences.
    pub fn find(&self, display: &str) -> Option<&Voice> {
        let display = self
            .aliases
            .iter()
            .find(|(old, _)| old == display)
            .map_or(display, |(_, new)| new.as_str());
        self.voices.iter().find(|voice| voice.display == display)
    }
}

fn load_custom_voices(path: &Path) -> Result<Vec<Voice>, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("cannot read custom voices {}: {error}", path.display()))?;
    let entries: Vec<CustomVoice> = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid custom voices {}: {error}", path.display()))?;
    entries
        .into_iter()
        .map(|entry| {
            let valid_id = !entry.id.is_empty()
                && entry.id.len() <= 64
                && entry
                    .id
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == ' ');
            if !valid_id {
                return Err(format!(
                    "custom voice id {:?} must be 1-64 ASCII letters, digits or spaces",
                    entry.id
                ));
            }
            if entry.voice.trim().is_empty() {
                return Err(format!("custom voice {:?} has an empty voice", entry.id));
            }
            Ok(Voice {
                label: entry.label.unwrap_or_else(|| entry.id.clone()),
                display: entry.id,
                api: entry.voice,
                gender: entry.gender,
                description: entry.description,
                language: None,
                random: false,
                aigc: false,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_alias_resolves_to_a_qwen_audio_voice() {
        let catalog = VoiceCatalog::new(Backend::QwenAudio, None).unwrap();
        for (old, new) in QWEN_AUDIO_ALIASES {
            let voice = catalog
                .find(old)
                .unwrap_or_else(|| panic!("{old} is dangling"));
            assert_eq!(voice.display, *new);
        }
    }

    #[test]
    fn adult_voice_ids_carry_the_gender_suffix() {
        for backend in [Backend::QwenAudio, Backend::QwenTts] {
            let catalog = VoiceCatalog::new(backend, None).unwrap();
            for voice in catalog.voices() {
                let child = voice.display.ends_with(" Boy") || voice.display.ends_with(" Girl");
                let suffix = match voice.gender {
                    Gender::Female => " Woman",
                    Gender::Male => " Man",
                };
                assert!(
                    child || voice.display.ends_with(suffix),
                    "{} does not end with {suffix}",
                    voice.display
                );
            }
        }
    }
}

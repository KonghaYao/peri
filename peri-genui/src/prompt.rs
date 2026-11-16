use crate::{builtins::FUNCTIONS, library::Schema, Library};

#[derive(Debug, Clone, Default)]
pub struct PromptOptions {
    pub preamble: Option<String>,
    pub additional_rules: Vec<String>,
    pub examples: Vec<String>,
}

pub fn system_prompt(library: &Library, options: &PromptOptions) -> String {
    let mut sections = Vec::new();
    if let Some(preamble) = &options.preamble {
        sections.push(preamble.clone());
    }
    sections.push("使用 OpenUI Lang 描述界面。必须把程序放在 gen-ui fenced block 中；开启 fence 后必须换行，禁止同行代码。程序第一条语句必须是 root；之后声明组件、state 和数据。".into());
    sections.push("语法：每条语句为 name = expression，占一行；括号及三元表达式内可换行。组件参数按下列签名的位置顺序传入，不使用命名参数。可省略末尾可选参数；中间空位写 null。可使用前向引用，但所有引用必须有定义；未被 root 引用的值不会显示。字符串使用双引号及 JSON 转义。".into());
    sections.push("表达式支持数组、对象、成员访问、数组成员提取、索引、+ - * / %、== != > < >= <=、&& || !、条件 ? 是 : 否及括号。$name = value 声明状态默认值；未声明的 $name 默认为 null。$binding 参数接受状态引用。".into());
    let mut components = vec!["## 组件签名（? 为可选参数）".to_owned()];
    for (name, component) in &library.components {
        let args = component
            .params
            .iter()
            .map(|p| {
                let ty = schema_type(&p.schema);
                let ty = if p.binding {
                    format!("$binding<{ty}>")
                } else {
                    ty
                };
                format!("{}{}: {}", p.name, if p.required { "" } else { "?" }, ty)
            })
            .collect::<Vec<_>>()
            .join(", ");
        components.push(format!("{name}({args}) — {}", component.description));
    }
    sections.push(components.join("\n"));
    sections.push(format!("## 内置函数\n{}\n@Each 的循环变量只在内联 template 内可用。数值函数接受数组；Round、Abs、Floor、Ceil 可逐项处理嵌套数组。",
        FUNCTIONS.iter().map(|(name, args)| format!("@{name}({args})")).collect::<Vec<_>>().join("\n")));
    sections.push("## 动作描述\n@ToAssistant(message, context?)、@OpenUrl(url)、@Set($name, value)、@Reset($name, ...) 返回动作描述，不在求值时执行。单个动作直接传入参数；多个动作用 Action([@Set(...), @ToAssistant(...)])；普通数组不会自动合并成动作。Set 的值保留到交互时求值；具体执行由宿主负责。".into());
    let root = library
        .root()
        .map(|s| format!("{s}(...) "))
        .unwrap_or_else(|| "组件名(...) ".into());
    sections.push(format!("输出格式（用组件签名替换省略号）：\n```gen-ui\nroot = {}\n```\n必须先输出 root，让子组件随流式定义逐步出现。", root.trim_end()));
    if !options.examples.is_empty() {
        sections.push(format!("## 示例\n{}", options.examples.join("\n\n")));
    }
    if !options.additional_rules.is_empty() {
        sections.push(
            options
                .additional_rules
                .iter()
                .map(|r| format!("- {r}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    sections.join("\n\n")
}

fn schema_type(schema: &Schema) -> String {
    if let Some(name) = schema.ref_name() {
        return name.to_owned();
    }
    if let Some(values) = &schema.enum_values {
        return values
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" | ");
    }
    if let Some(value) = &schema.const_value {
        return value.to_string();
    }
    let choices = if schema.any_of.is_empty() {
        &schema.one_of
    } else {
        &schema.any_of
    };
    if !choices.is_empty() {
        return choices
            .iter()
            .map(schema_type)
            .collect::<Vec<_>>()
            .join(" | ");
    }
    match schema.ty.as_deref() {
        Some("array") => format!(
            "{}[]",
            schema
                .items
                .as_deref()
                .map(schema_type)
                .unwrap_or_else(|| "any".into())
        ),
        Some(ty) => ty.to_owned(),
        None => "any".into(),
    }
}

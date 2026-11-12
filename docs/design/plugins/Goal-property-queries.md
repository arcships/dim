# Goal：业务状态读取与输入删除

Status: Draft.
Scope: Goal 加载查询、旧历史和输入删除边界。
Issue: [#1320](https://github.com/arcships/dim/issues/1320)。

目标状态、写入及重载暂停由 [Goal 状态设计](Goal-state-read.md)规定。

## 1. 加载读取

固定本分支已发布读头 S。通过声明的 timeline.query，结束分支通过 treeCommands.query，执行两次逻辑查询：

1. branch/start、session/forked 合并范围中的最后一条位置；不按 kind 分组、不统计历史条数。
2. 本地 opening 至 S 的 goal/state，Last、limit=1，只取 position.sequence 和 data。

Store 复用已有类型/位置索引。只解码一个完整业务状态；没有该事件则没有 Goal。未知或损坏 schema 报错，不冒用更旧状态。加载不读取 input/message、blocks、submission_id、删除位置、旧轮次或旧恢复快照。没有候选分页、配对点查、删除关联或来源后缀重放。

active 在内存映射为 paused，reason=interrupted_reload；其余业务状态保留。返回 S 后由 Dispatcher 交接新事实，加载零写入。活 get 读取原 owner 内存，不查询历史。

## 2. 旧历史

只有旧 goal:state、goal:objective、goal:status 文本或旧 goal/recovered 的历史不产生当前 Goal。旧事实保留供历史、导出和原上下文使用；不迁移、不回填、不恢复旧目标或执行。当前 Goal 仍正常生产模型协议文本；新分支退休继承 key 的规则不变，那是模型上下文清理，不是旧目标恢复。

## 3. 输入删除

Goal 状态与必要模型输入同批提交即生效。input/deleted 表示 InputQueue 删除其负责的输入，不撤销已经生效的 Goal 业务状态。

Goal 只从本实例 driving、continuation_inputs 移除该 input_id，随后按当前实例正常等待/续行条件处理。objective、state、judge 不变；不查历史、不重算目标、不取消已有决定、不保存补偿状态。删除 set、edit、pause、resume、clear 的 trigger 均遵守同一规则。明确改变目标仍通过 Goal 操作，执行异常仍通过自己的终态。

删除前后重载均读取最新一条 goal/state，并应用同一个重载暂停规则。没有修复窗口、修复锁、有效命令索引或每输入恢复记录。

## 4. 验收与删减

覆盖：最新业务状态、未知 schema、本地 opening/fork、仅旧文本时无 Goal、删除命令输入不改变已提交目标、删除后重载不回滚、普通输入删除仍解除当前等待。查询只返回必要目标，不因普通历史增长增加 Goal 候选或删除读取。

删除 legacy/pair、删除后 refresh、状态重放、补偿写入、修复等待。保留现有模型文本生产、当前输入 ID 集合和异常终态，分别服务模型通知、本次执行等待和资源结算。

## Rule check

Goal branch 是原 Capability；作用域、声明依赖和配置沿用 [Goal](Goal.md)。principles §4 规则 1、2：输入删除由 InputQueue 决定，Goal 只处理自己的当前 ID；规则 3、4：无 before I/O、无 Store 业务解释；规则 5：原业务状态与必要输入原子提交；规则 6：上下文仍来自采用输入；规则 7、8：原命名服务，无新机制；规则 9：不保存兼容进度或删除恢复状态。ADR 002 D1–D5 不改变。

## Requirements raised to mechanism owners

无。复用已有 TimelineQuery、Fields、Last 和类型/位置索引，不增加 API、Store 表、格式版本或其他插件依赖。

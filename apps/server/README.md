# 服务端协调

包名：`torse-server`。B4提供串行协调器、世界目录锁和文件生命周期；原WorldHost继续服务可信本地事务。B6新增ExecutionHost、ExecutionRecovery与完整持久阶段驱动，B7新增NetworkHub、授权复制和内存/非阻塞TCP接入，主程序已可启动。范围与证据见[执行/网络验收](../../benchmarks/B6-B7-execution-network.md)。

| 文件 | 职责 |
|---|---|
| [authority.rs](src/authority.rs) | 服务端本地权威、准备项、独立恢复和订阅失效适配接口 |
| [checkpoint.rs](src/checkpoint.rs) | 检查点保留、维护与存储健康状态适配 |
| [coordinator.rs](src/coordinator.rs) | 预留、发布、接纳、刷盘、持久确认、检查点与有界关闭 |
| [directory.rs](src/directory.rs) | 世界目录排他锁与运行代数分配 |
| [lifecycle.rs](src/lifecycle.rs) | 文件世界创建、只读恢复检查、激活与日志重开 |
| [world.rs](src/world.rs) | 真实世界的可信本地事务、完整记录、有界快照与独立恢复适配 |
| [execution.rs](src/execution.rs) | 持久阶段工作、定时事件、动作结果、去重及完整执行恢复 |
| [network.rs](src/network.rs) | 会话策略、输入适配、持久输出门槛、订阅与失效控制 |
| [bootstrap.rs](src/bootstrap.rs) | 有限演示世界及显式容量配置 |
| [main.rs](src/main.rs) | 非阻塞监听、固定逻辑Tick和有界关闭 |

## B6/B7执行世界

```sh
cargo run -p torse-server -- .work/execution-world --listen 127.0.0.1:7777 --ticks 60
cargo run -p torse-server --example headless_client -- 127.0.0.1:7777
```

第二条在另一终端执行。--ticks 0持续运行，有限Tick结束后保存并关闭；同目录再次启动恢复Tick、事件、结果和任务位置，使用新epoch。演示创建两个Section及两个容器，对新连接使用只读访客PeerPolicy，因而headless_client看到实体但看不到库存组件。

ExecutionHost独占A7 ReplicaSource，使用纯DurableRule规划系统和事件。阶段先写完整冻结任务，再逐项原子记录世界变化、任务进度、事件和结果，最后推进一个阶段。未来输入的关闭拒绝可以使用独立Stopping记录，不越过截止边界执行下一Tick。旧Simulation保留参考用途，不能在日志外直接执行后再补写进度。

持久数据使用ExecutionCodec<ExecutionModel>，格式见[EXECUTION.md](../../crates/codec/EXECUTION.md)。这是新执行目录，原WorldCodec世界目录不自动迁移；不兼容数据会报错并保留。完整状态每条日志替换、有限世界驻留、同步刷盘是本轮边界，未进行吞吐或硬件断电验收。

NetworkHub通过泛型协议接收意图，可信actor由PeerPolicy和会话绑定。结果、查询、基线和增量都经过DurableSeq门槛。撤权控制直接清空未发队列并关闭受影响Peer，慢连接不阻塞世界。真实用户认证、TLS、窗口和移动预测由应用后续集成。

## 原本地世界工具

WorldHost消耗一个初始化完成的WorldAuthority，后续只提供只读world访问。Server::commit_transaction接收借用的可信本地事务，准备时生成完整ChangeSet，仍由通用协调器执行预留、发布、接纳和刷盘。LocalCommit分别表示业务拒绝、存储预留受阻或持久提交。预留受阻时世界、序号、实体分配水位和调用方事务均保持原状，可重新调用。

可信本地事务现已支持实体生成与销毁，可与其他实体的移动、组件修改及块编辑成组提交。生成身份在准备阶段按顺序提出，发布时才消耗；销毁保存完整旧状态并复核组件所有权。相同实体不能在一条记录中同时生成或销毁并移动或编辑组件。旧的块专用、无模式入口会拒绝生命周期工作，不能静默丢弃。

生成位置须在可读区域，坐标每轴不超过正负1048576格；生成记录不隐含碰撞体。每事务生成和销毁合计上限256，生命周期保守载荷上限8MiB，实际日志预算仍可更低。ActionService在动作权限扩展前拒绝处理器输出生命周期计划，ActionOutput::Mutation现持有Box<Transaction>。

Server::advance_phase在可信调用方完成当前阶段工作后，经相同日志门槛持久推进一个完整阶段。阶段0到5顺序推进，发布阶段完成后进入下一Tick的输入阶段，next_step保持0。进度记录使用空ChangeSet；普通世界事务保持当前恢复位置。预留受阻不改变进度，刷盘失败停止实例并禁止读取。该接口不调用Simulation或玩法系统，调用方仍负责确认阶段工作已完成。

WorldHostRecovery允许从日志恢复到Tick中间的完整阶段边界，拒绝跳阶段、倒退、非零步骤及同条记录同时推进和修改世界。检查点仍只在Tick起点捕获，关闭时把当前Tick剩余阶段依次写入日志，再保存下一Tick起点。LocalWorldModel的网络输入、输出和复制载荷使用Infallible，网络接纳入口无法构造请求。TransactionId只供诊断，不提供请求去重语义。待执行事件、会话、动作结果和复制订阅继续由后续阶段接入。

capture_snapshot_with_budget先借用世界检查条目和字节预算，再捕获完整快照。条目包括根、状态、分区、实体、组件、字段和嵌套集合项；字节包括结构、集合元素、字符串和稠密格值，宿主另计封装元数据。预算不包含分配器开销和临时捕获索引，编码预算与文件容量继续单独检查。世界全部驻留，运行期没有可变存储或卸载入口。

在实现仓库运行以下命令，再运行两次可验证恢复、销毁和身份续号。示例只使用基础内容夹具，每次切换同一格的石头与空气，并生成或销毁一个空组件实体，成功后保存检查点。新写入采用世界格式3，读取继续接受版本2数据。

```sh
cargo run -p torse-server --example durable_world -- .work/local-world
```

[真实世界测试](tests/world.rs)覆盖混合事务与库存争用、检查点和日志尾恢复、超限后重试、刷盘失败门槛、领域拒绝时保持文件尾部，以及两个进程中止位置。详细结果见[验证记录](../../benchmarks/M3-local-world.md)。

阶段推进的逐阶段恢复、受阻重试、有界关闭、非法进度及进程退出验证见[阶段记录](../../benchmarks/M3-phase-progress.md)。

## 提交与访问

Server拥有权威实例、日志、检查点和目录锁。HostAuthority是应用层适配接口，未替换A的WorldAuthority、Recovery或共同契约。准备项必须拥有完整ModelCommit，丢弃后保留输入、事件、序号与持久身份，不能只提供CommitMeta。

drive_one依次准备、预留日志、发布、立即接纳日志、处理订阅失效、刷盘，最后返回DurablePublication。日志预留的Busy、TooLarge、InvalidRecord发生在发布前，返回Blocked，调用方可以处理背压或拒绝原因后重新准备。Stopped、发布不变量故障、接纳失败、失效处理失败和刷盘失败停止实例，不再次发布，也不返回持久确认。

刷盘结果必须匹配完整Binding，覆盖目标，且不超过权威位置。发布位置、恢复位置与准备记录也必须一致。服务端不提供可变权威引用；只读authority访问要求实例正常且当前位置已持久。submit先拒绝无关世界和旧运行代数。ExecutionHost与NetworkHub已负责会话身份、权限、速率及持久去重；Queued仍不构成动作成功承诺。

订阅失效通过独立InvalidationSink处理，发生在释放输出之前。没有复制连接时使用NoReplicas，收到非空失效列表会停止实例，避免漏处理。ExecutionHost/NetworkHub已接入普通结果和复制队列，原LocalWorldModel继续使用NoReplicas。FlushMetrics单独记录日志屏障调用次数及耗时，包含无须追加的调用，不混入权威执行时间。

## 文件启动

先校验FileServerConfig，再用WorldDirectory取得已存在根目录的排他锁。调用方负责首次创建根目录及其上级目录项的持久发布。Unix分支会同步已有根目录中的新文件与子目录项，Windows使用文件sync_all，硬件断电保证仍待平台验收。

create_file拒绝既有日志或检查点目录，创建并同步空日志，再保存位置0的完整初始快照。初始化成功后才返回可接纳输入的Server，失败不会开放半初始化世界。

open_file先检查文件与兼容信息，用Checkpoints::inspect读取完整manifest及快照。独立恢复构建器应用快照，再扫描完整日志，仅向构建器重放快照之后的连续记录。日志必须覆盖快照位置及manifest中已知的DurableSeq。构建器finish校验预期位置，返回的权威实例也要匹配新Binding和扫描位置。

宿主同时保留最终ResumePoint：存在快照后的完整记录时取最后一条，否则取快照值。finish返回的Tick、阶段和步骤必须全部匹配，再激活存储，避免只恢复正确序号却重置执行进度。

领域恢复通过后才activate检查点，并重开日志完成不完整尾部修复与同步。领域拒绝、完整记录损坏、配置错误及预期位置不足时，快照、manifest与日志内容保持原状；本次尝试已消耗的运行代数仍保留。激活期间I/O失败可能留下已修复的合法尾部，实例仍不会开放。恢复不调用玩法处理器，不发送成功通知。

## 检查点维护

checkpoint_predecessors规定每次发布新快照前保留最近多少代已发布快照，至少为1。FileServerConfig要求快照文件容量严格大于该值，为新快照预留一个文件位置。每次捕获并核对快照后，先清理更旧快照和孤立文件，再发布新一代；当前检查点和全部B2日志始终保留，发布成功后至多保留前代数加1份快照。

HostCheckpointStore是应用本地维护适配接口，沿用共同CheckpointStore的发布签名。prune_checkpoints提供目录锁保护下的显式清理入口。清理不删除日志，不改变当前权威状态。

健康存储在写入前报告ResourceLimit时，checkpoint返回错误，Server仍可运行或维护后重试。若容量错误发生在部分写入之后，存储已停止，宿主仍按终止性故障处理。manifest总容量继续保持显式上限，当前没有自动压缩；关闭时无法发布检查点仍返回失败，不宣称保存成功。

## 目录与运行代数

```text
world/
    world.lock
    journal.log
    checkpoints/
        manifest.log
        checkpoint-0000000000000001.bin
```

world.lock同时是持有整个运行期的排他锁和追加式运行代数记录。文件保持原位，关闭后也不删除或替换。每次取得锁后验证记录，分配上一个代数加1，写入并同步后才返回WorldDirectory。使用同一世界目录的多进程互斥，普通路径别名也锁定同一个文件。目录锁是协作协议，不防御绕过锁的外部修改或同时运行的世界目录副本。

每条记录固定48字节，代数从1开始，各整数为小端。

| 偏移 | 字节数 | 内容 |
|---|---|---|
| 0 | 8 | magic与版本，ASCII文本TORSEEP1 |
| 8 | 16 | WorldId |
| 24 | 16 | WorldEpoch |
| 40 | 4 | CRC32C，覆盖前40字节 |
| 44 | 4 | 结束标记，ASCII文本EEND |

完整记录损坏、世界不符或代数不连续均拒绝，不回退。末尾未写完整的分配尚未对外返回，可以验证前缀后修复。max_epoch_bytes限制记录总长度，容量耗尽明确失败，不循环复用，不自动压缩或重置。校验复用`crc32c = "=0.6.8"`。

## 关闭

shutdown消耗Server，先停止接纳，通知权威适配器按包含截止Tick的规则处理已接纳输入。截止点之后的未执行输入返回Stopping，未来机器事件仍留在世界中。适配器必须把必要的Tick收尾推进形成日志，并在输入处理完且到达Tick边界时报告shutdown_ready。

关闭推进受shutdown_max_steps限制。持续Busy、无法预留、无进展、越过截止边界及其他故障都会返回失败。成功路径先完成日志屏障，再捕获完整快照并发布检查点，最后释放权威、日志和检查点句柄，目录锁最后释放。失败也会释放资源，不返回保存成功。

允许推进到截止Tick之后的第一个Tick起始位置，禁止进入该Tick内部的阶段或非初始步骤。HostAuthority::is_tick_start按锁定规则图判断候选ResumePoint，宿主在准备项发布前执行检查，避免关闭报错之前已经写入超出截止范围的变化。

## 验证与后续

[协调器测试](tests/coordinator.rs)检查调用次序、预留拒绝后的重试、失败后的持久门槛、输入身份、订阅失效、快照与有界关闭。[目录测试](tests/directory.rs)检查跨进程互斥、退出后解锁、逐字节截断与位翻转、代数递增和容量。[文件生命周期测试](tests/lifecycle.rs)覆盖恢复后续号、尾部保留、整条后缀缺失、初始化拒绝，以及发布后、接纳前突然退出。

当前B2日志从位置1完整保留，恢复会校验整个文件。ExecutionHost和NetworkHub已补齐持久阶段任务、事件、请求结果、去重与授权连接，见[执行测试](tests/execution.rs)。分段回收、异步刷盘、脏区流送和硬件断电保证仍需单独验收，当前功能测试不代替性能或平台承诺。

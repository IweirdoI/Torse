# 服务端

包名：`torse-server`。提供权威世界的持久执行、文件恢复、授权网络和可选Mod装配，可在无GPU环境中运行。主程序通过ExecutionHost驱动阶段任务，WorldHost保留可信本地事务示例。范围与证据见[执行与网络验证](../../benchmarks/B6-B7-execution-network.md)。

| 文件 | 职责 |
|---|---|
| [authority.rs](src/authority.rs) | 服务端本地权威、准备项、独立恢复和订阅失效适配接口 |
| [checkpoint.rs](src/checkpoint.rs) | 检查点保留、维护与存储健康状态适配 |
| [coordinator.rs](src/coordinator.rs) | 预留、发布、接纳、刷盘、持久确认、检查点与有界关闭 |
| [directory.rs](src/directory.rs) | 世界目录排他锁与运行代数分配 |
| [lifecycle.rs](src/lifecycle.rs) | 文件世界创建、只读恢复检查、激活与日志重开 |
| [world.rs](src/world.rs) | 真实世界的可信本地事务、完整记录、有界快照与独立恢复适配 |
| [execution.rs](src/execution.rs) | 持久阶段工作、定时事件、动作结果、去重及完整执行恢复 |
| [profiling.rs](src/profiling.rs) | 串行执行的可选计时接线 |
| [planning.rs](src/planning.rs) | 冻结前的独立规则并行与稳定归并 |
| [prevalidation.rs](src/prevalidation.rs) | 冻结后的有界移动与组件预验证及串行提交前复核 |
| [network.rs](src/network.rs) | 会话策略、输入适配、持久输出门槛、订阅与失效控制 |
| [bootstrap.rs](src/bootstrap.rs) | 有限演示世界及显式容量配置 |
| [main.rs](src/main.rs) | 非阻塞监听、固定逻辑Tick和有界关闭 |

## 串行计时

`Server::set_serial_profiler`可接入有界的`torse_diagnostics::SerialProfiler`，默认关闭。规划与准备、复制投影与发布分别计时并扣除嵌套成本，日志另计编码字节与刷盘耗时。计时只服务诊断，不改变规则或持久数据。

```sh
cargo run --release -p torse-server --example serial_baseline --locked -- --output .work/serial-baseline --ticks 30 --warmup 5
```

基准对照无存储sink、保留日志的内存后端和真实文件后端，覆盖空Tick、不同任务数、争用与订阅数量。输出目录必须不存在；原始CSV、边界说明和本机测量见[串行基线](../../benchmarks/M6-serial-baseline.md)。

## 冻结前并行规划

`ExecutionHost::set_parallel_planning(Some(PlanningConfig { workers: 4, max_in_flight: 4 }))`启用有界批次；Server提供同名入口，主程序可传`--planning-workers 4`。默认保留串行执行。规则通过`DurableRule::parallel_planner`返回`Some(self)`显式加入，编译器要求其实现Sync。现有Mod适配器继续在宿主线程执行。

请求与定时事件先按原次序规划，周期规则只在相邻且没有声明依赖时并行。工作线程借用同一阶段起点世界，批次全部结束后按原拓扑次序归并，再使用原稳定事务排序。每批最多max_in_flight项，原生规划器须遵守阶段和运行态预算。冻结缓存、逐项准备、日志接纳、发布、刷盘和恢复均沿用现有路径。

借用批次通过torse-runtime的run_scoped执行，每批创建并回收有限线程；轻规划会增加开销，启用前应实测。`cargo run --release -p torse-server --example parallel_baseline -- --output .work/parallel-baseline`生成串行、1和4线程对照，详见[并行规划记录](../../benchmarks/M6-parallel-planning.md)。

## 移动预验证

`ExecutionHost::set_prevalidation(Some(PrevalidationConfig::default()))`启用冻结任务的只读碰撞预计算，Server提供同名入口。主程序使用`--prevalidate-workers 4`，默认关闭。默认最多4线程、16项在途任务，每项预留8MiB；预计扫掠体积不足512格的轻任务继续串行准备。

只处理相邻、不同实体、单成员且仅含一次移动的任务。显式块前置、块写入、组件、生成、销毁与重复实体会截断批次。工作线程返回不可直接发布的MovementPrevalidation；宿主逐项检查阶段、兼容信息、碰撞读集、实体身份代数和精确前位置，再绑定当前提交序号。过期、超限或执行器失败均回到原串行准备，拒绝原因由原路径确定。

缓存只在内存中存在，已发布任务立即释放，表或阶段变化时清空，文件恢复重新计算剩余冻结任务。日志Busy重试复用缓存；提交、事件结算、撤权、批量刷盘及持久输出继续串行。`prevalidation_stats`报告预计算数、命中、过期、跳过及预留和缓存峰值，命中计数包含未发布重试。

大碰撞体负载有实测收益，小碰撞体默认跳过。完整路径、原始数据及真实单机器Mod对照见[移动预验证与投影](../../benchmarks/M6-movement-prevalidation.md)。

## 组件预验证

组件事务可通过`--prevalidate-workers 4 --prevalidate-components`显式启用，默认关闭。该CLI组合将每项工作及结果预留设为64MiB，默认窗口16项，最大预留1GiB；实际内存不等于预留。API对应`PrevalidationConfig.components`、`min_component_bytes`及`max_job_bytes`。默认跳过前后组件载荷合计不足16KiB的任务。

支持仅含组件修改的单成员或跨模块组，各任务涉及的实体必须互不重叠。显式块前置、块写入、移动、生成和销毁截断窗口。工作线程按原所有权和模式约束准备，宿主提交前复核世界、阶段、兼容信息、身份代数、旧字段与最终组件容量。令牌按所有权移入准备项，避免再复制完整字段树；日志Busy导致未发布准备项丢弃后，该项按原串行路径重试。恢复丢弃缓存并处理剩余冻结任务。

`prepare_group`同时复用成员已经完成形状校验的ComponentBatch，合并时继续检查重复目标、总量、旧值与最终容量，保留原成员拒绝次序。该优化始终启用。组件并行在完整路径中的收益有限，P95也可能变差，应按实际负载测量后启用。`peak_cached_bytes`对组件令牌记录保守工作计费，不表示实际常驻内存。对照与范围见[组件预验证记录](../../benchmarks/M6-component-prevalidation.md)。

## 授权增量投影

复制源默认根据提交实际修改的分区和实体更新订阅，只重建改变的Section及实体可见组件集合。状态引用计数维护完整可见字典，缓存成本继续检查整个投影的条目和字节上限；队列也在克隆载荷前检查。每个可见提交仍输出一个原子批次，撤权、慢订阅重置和DurableSeq发送门槛保持原语义。

`Server::set_projection_optimization(false)`可切换到完整投影作诊断对照，ExecutionHost及ReplicaSource也提供同名入口。重新启用时从当前订阅基线重建派生缓存。缓存不进入存档或协议；本轮不提供运行期地形装卸。测量、容量边界和编码一致性验证见[增量投影记录](../../benchmarks/M6-incremental-projection.md)。

## 启动与连接

```sh
cargo run -p torse-server -- .work/execution-world --listen 127.0.0.1:7777
cargo run -p torse-server --example headless_client -- 127.0.0.1:7777
```

第二条在另一终端执行，需保持服务端运行。默认或`--ticks 0`持续运行，指定`--ticks 60`会在60个Tick后保存并关闭；同目录再次启动恢复Tick、事件、结果和任务位置，使用新epoch。演示创建两个Section及两个容器，对新连接使用只读访客PeerPolicy，因而headless_client看到实体但看不到库存组件。`--fill`仅用于新世界，把两个演示分区铺一层石头地面，方便图形客户端连上后有地形可见；已有存档的世界使用该参数会报错。`--editors`对新连接授予订阅分区内的编辑权限（ActorContext分区授权），配合图形客户端可完成经服务端提交的放置与破坏。`--revoke-after N`以首个连接为锚点，N个Tick后撤销全部会话并关闭传输，用于验收撤权与重连；行为与验证见[授权验收](../../benchmarks/A8-acceptance.md)。

ExecutionHost独占ReplicaSource，使用纯DurableRule规划系统和事件。阶段先写完整冻结任务，再逐项原子记录世界变化、任务进度、事件和结果，最后推进一个阶段。未来输入的关闭拒绝可以使用独立Stopping记录，不越过截止边界执行下一Tick。旧Simulation保留参考用途，不能在日志外直接执行后再补写进度。

WorkPlan支持携带TransactionGroup。Mod机器自有状态与库存规划器返回的事务按来源组成一个任务，连同新唤醒共同预留、发布和刷盘。冻结任务保存稳定成员，恢复不重跑已规划或已结算的Guest。库存拒绝保留机器完成前的进度，不登记该计划的新唤醒；陷阱重试次数由持久事件载荷记录，停机以没有后续唤醒表示。ModStats只供诊断，不影响恢复后的执行。验证见[Mod原子执行](../../benchmarks/M4-mod-atomic.md)。

持久数据使用ExecutionCodec<ExecutionModel>，格式见[EXECUTION.md](../../crates/codec/EXECUTION.md)。原WorldCodec世界目录不自动迁移；不兼容数据会报错并保留。提交载荷写版本3、读版本1至3，快照写版本2、读版本1和2，应用协议为版本2。冻结列表只在完整记录中保存，后续进度记录替换有界事件与请求窗口。主程序仍使用有限范围，显式装卸接口另见下文，未进行硬件断电验收。

## 运行态与批量刷盘

默认共享不可变规则及冻结任务表，缓存其编码和解码预算成本；动态事件、结果及全运行态上限继续校验。逐任务记录保存进度，恢复仍重新核对终态，不重跑规则。`Server::set_runtime_optimization(false)`可切换到完整运行态记录和完整编码/解码校验作对照，内存表共享仍保留。

事件预算按身份和完整内容复用真实编解码成本，变化后重新测量，删除后移除缓存；事件合法性及完整运行态上限仍每次检查。缓存项受事件数限制，拥有载荷计费不超过运行态字节上限；预算与编码限制变化使缓存失效。进度记录直接构造，避免先复制完整运行态。实际1、16、64台Wasm机器的负载、争用与恢复对照见[真实多机器测量](../../benchmarks/M6-multi-machine.md)。

`drive_one`继续每条记录同步。`drive_batch`逐项执行准备、日志预留、发布、立即接纳及失效控制，在达到记录上限、Tick边界、空闲或容量阻塞时同步已接纳前缀，然后返回CommitBatch。追加或同步失败时停止实例，不返回该批成功，结果、基线、复制与快照入口关闭。容量Busy时先同步已接纳部分，后续可以继续重试。

Server的Tick驱动默认每批1条，可用`set_commit_batch_limit`设置1到1024。主程序默认16条，并设置相应日志槽数；`--commit-batch 1`可回到逐条同步。任何成功返回都要求该批完整前缀通过DurableSeq门槛，未确认的内存批次在进程退出后可能丢失。关闭流程仍有界排空并保存检查点。

复现对照与故障验证见[运行态与批量提交](../../benchmarks/M6-runtime-commit.md)。当前简单事务的并行预验证实验未显示收益，因此继续使用串行事务准备与发布。

NetworkHub通过应用协议版本2接收意图，可信actor由PeerPolicy和会话绑定。结果、查询、基线和增量都经过DurableSeq门槛。撤权控制直接清空未发队列并关闭受影响Peer，慢连接不阻塞世界。图形客户端已接入授权世界与编辑请求；真实用户认证、TLS，以及服务端移动结算和客户端校正尚未接入。

## 本地世界示例

WorldHost消耗一个初始化完成的WorldAuthority，后续只提供只读world访问。Server::commit_transaction接收借用的可信本地事务，准备时生成完整ChangeSet，仍由通用协调器执行预留、发布、接纳和刷盘。LocalCommit分别表示业务拒绝、存储预留受阻或持久提交。预留受阻时世界、序号、实体分配水位和调用方事务均保持原状，可重新调用。

可信本地事务现已支持实体生成与销毁，可与其他实体的移动、组件修改及块编辑成组提交。生成身份在准备阶段按顺序提出，发布时才消耗；销毁保存完整旧状态并复核组件所有权。相同实体不能在一条记录中同时生成或销毁并移动或编辑组件。旧的块专用、无模式入口会拒绝生命周期工作，不能静默丢弃。

生成位置须在可读区域，坐标每轴不超过正负1048576格；生成记录不隐含碰撞体。每事务生成和销毁合计上限256，生命周期保守载荷上限8MiB，实际日志预算仍可更低。ActionService在动作权限扩展前拒绝处理器输出生命周期计划，ActionOutput::Mutation现持有Box<Transaction>。

Server::advance_phase在可信调用方完成当前阶段工作后，经相同日志门槛持久推进一个完整阶段。阶段0到5顺序推进，发布阶段完成后进入下一Tick的输入阶段，next_step保持0。进度记录使用空ChangeSet；普通世界事务保持当前恢复位置。预留受阻不改变进度，刷盘失败停止实例并禁止读取。该接口不调用Simulation或玩法系统，调用方仍负责确认阶段工作已完成。

WorldHostRecovery允许从日志恢复到Tick中间的完整阶段边界，拒绝跳阶段、倒退、非零步骤及同条记录同时推进和修改世界。检查点仍只在Tick起点捕获，关闭时把当前Tick剩余阶段依次写入日志，再保存下一Tick起点。LocalWorldModel的网络输入、输出和复制载荷使用Infallible，网络接纳入口无法构造请求。TransactionId只供诊断，不提供请求去重语义。待执行事件、动作结果与请求去重由独立的ExecutionHost处理，会话与订阅由NetworkHub管理；此本地模型不接入网络。

capture_snapshot_with_budget先借用世界检查条目和字节预算，再捕获完整快照。条目包括根、状态、分区、实体、组件、字段和嵌套集合项；字节包括结构、集合元素、字符串和稠密格值，宿主另计封装元数据。预算不包含分配器开销和临时捕获索引，编码预算与文件容量继续单独检查。世界全部驻留，运行期没有可变存储或卸载入口。

在实现仓库运行以下命令，再运行两次可验证恢复、销毁和身份续号。示例只使用基础内容夹具，每次切换同一格的石头与空气，并生成或销毁一个空组件实体，成功后保存检查点。新写入采用世界格式3，读取继续接受版本2数据。

```sh
cargo run -p torse-server --example durable_world -- .work/local-world
```

[真实世界测试](tests/world.rs)覆盖混合事务与库存争用、检查点和日志尾恢复、超限后重试、刷盘失败门槛、领域拒绝时保持文件尾部，以及两个进程中止位置。详细结果见[验证记录](../../benchmarks/M3-local-world.md)。

阶段推进的逐阶段恢复、受阻重试、有界关闭、非法进度及进程退出验证见[阶段记录](../../benchmarks/M3-phase-progress.md)。

## 提交与访问

Server拥有权威实例、日志、检查点和目录锁。HostAuthority是应用层适配接口，通过WorldAuthority、Recovery及公共契约接入领域实现。准备项必须拥有完整ModelCommit，丢弃后保留输入、事件、序号与持久身份，不能只提供CommitMeta。

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

当前日志从位置1完整保留，恢复会校验整个文件。ExecutionHost和NetworkHub已补齐持久阶段任务、事件、请求结果、去重与授权连接，见[执行测试](tests/execution.rs)。分段回收、异步刷盘、脏区流送和硬件断电保证仍需单独验收，当前功能测试不代替性能或平台承诺。

## Mod机器与UI模型

启用`mods`后可用`--mod DIRECTORY`重复加载包；创建和重开均须使用相同内容。显式`--machine-operators`演示策略对所有新连接授予机器及输入、输出端口读取和库存转移权限，默认不授予。图形客户端还需加载匹配内容并消费`torse_mod_api::ui`模型；双图形客户端的真实机器操作仍待接入。

```powershell
cargo run -p torse-server --features mods -- .work/machine-ui-world --mod examples/mods/machine --machine-operators
cargo run -p torse-server --features mods --example headless_client -- 127.0.0.1:7777 --mod examples/mods/machine
```

以上两条命令分终端运行；首次使用前按[Mod说明](../../examples/mods/README.md)安装Wasm目标。重新打开同一世界时须保留相同包与内容。

公共模型从授权镜像构建进度与库存摘要，按钮复用`inventory:action/transfer`。`DurableRule::action_events`在冻结动作时规划方块通知，通知与动作共同持久结算，冲突和重发不重复派发。绑定方式为包数据的`[[block]] behavior = true`。复现、真实进程退出验证及限制见[机器UI验证](../../benchmarks/M4-machine-ui.md)。

## 客户端心跳

握手后Ping只刷新接收活跃时间，不回复、不占请求序号，也不写入世界日志。握手前Ping关闭对应连接，旧应用协议版本1在Hello时拒绝。wire版本与存档布局保持不变。

图形客户端每10秒发送一次，从渲染帧和等待钩子驱动，最小化窗口仍轮询网络。演示启动检查`idle_ticks * tick_period >= 3 * heartbeat_interval`：默认1200个Tick、50毫秒一个Tick，空闲窗口60秒。NetworkLimits::validate_timing供其他真实时间宿主在装配时核对配置；使用虚拟时钟的测试可自行安排节奏。静默连接继续回收。验证见[心跳记录](../../benchmarks/B7-heartbeat.md)。

## 持久分区装卸

ExecutionHost已接入Server::unload_section、reload_section和install_section。调用方先持有世界目录锁，再装配SectionPages；恢复器使用with_section_pages接入同一页目录。页先同步发布，装卸记录再走原日志门槛；受影响的授权Peer通过独立控制失效，未相交连接保持。

装卸限于没有已接纳输入或冻结规划的Tick起点，未来事件保守阻止卸载，票据继续生效。未驻留页保留实体身份和持久字段，活动表移除对象，重新装回后水位不变。分区页与未驻留索引均有容量限制，当前不自动回收孤立页。备份须包含日志、检查点及被引用的sections页；便携归档要求先装回全部分区。

可运行durable_stream示例逐次验证park、load和grow，命令、格式与场景证据见[持久分区装卸](../../benchmarks/M3-section-streaming.md)。自动选区、异步I/O及生成版本迁移另行实现。

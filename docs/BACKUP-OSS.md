# 在线备份与 OSS

`harness-backup` 是独立 Rust 工具，直接读取部署机的数据卷，日常 Agent 不需要额外调用。它不占用服务 writer 锁、不停机、不重启服务，也不修改源数据；手动命令与每天定时任务使用同一 JSON 配置。OSS 上传使用官方 `ossutil`，认证与分片传输由该工具处理。

## 安装与配置

优先在GitHub或独立构建机生成备份二进制，部署机只安装产物，不在小型业务服务器现场编译。需要源码安装时在构建机执行；`--no-default-features`只构建存储/备份部分，避免在小机器上编译HTTP/TLS依赖。已有服务可以继续运行，不需要为备份切换镜像：

```sh
cargo install --path . --locked --no-default-features --bin harness-backup
sudo install -m 755 "$HOME/.cargo/bin/harness-backup" /usr/local/bin/harness-backup
sudo install -d -m 700 /etc/harness-server /var/backups/harness-server
sudo install -m 600 examples/backup.example.json /etc/harness-server/backup.json
```

模板中 `data_dir` 和 Bucket 必须改为实际值；更换数据卷或恢复切换后同步更新 `data_dir`。Docker named volume 的数据目录可只读查询：

```sh
docker inspect <实际Harness容器名> \
  --format '{{range .Mounts}}{{if eq .Destination "/data"}}{{.Source}}{{end}}{{end}}'
```

不要用完整 `docker inspect` 输出查配置，里面可能含服务 token。原生服务填写实际 `--data-dir` 目录。`output_dir` 必须位于数据目录之外，也不能是其祖先；工具拒绝嵌套和符号链接。备份以 UTC 时间和随机 UUID 命名，目录 Unix 权限700、归档/清单600。

`oss.enabled=false` 默认只生成本地备份。填好 `bucket`、HTTPS `endpoint`、不带开头/结尾斜线的 `prefix`、`ossutil` 路径后，单独配置 OSS 凭据，再将 enabled 改为 true。JSON 只保存凭据文件路径，不保存 AccessKey 值。也可将 credentials_file 设为 null，让 ossutil 使用其默认凭据来源；定时任务的运行用户必须同样有权使用该来源。

安装官方 ossutil，并在部署机交互配置：

```sh
ossutil config --config-file /etc/harness-server/ossutil.credentials
chmod 600 /etc/harness-server/ossutil.credentials
```

参考 [官方配置文档](https://www.alibabacloud.com/help/en/oss/developer-reference/configure-ossutil) 和 [cp 文档](https://www.alibabacloud.com/help/en/oss/developer-reference/cp-1)。不同版本的地区/认证字段按所安装版本的 config 引导填写；先检查 `ossutil version`、`ossutil cp --help`。不要把凭据发给 Agent、放进 Git 或复制到备份 JSON。使用私有 Bucket；备份包含个人偏好、交接与执行记录。服务 token 不属于 OSS 凭据。

## 手动备份与上传重试

```sh
# 依据 enabled 设置决定是否上传；成功输出JSON，失败退出非零。
harness-backup --config /etc/harness-server/backup.json create

# 即使启用了OSS，也可只生成本地备份。
harness-backup --config /etc/harness-server/backup.json create --local-only

# 使用create返回的实际清单路径核验本地归档的大小和SHA-256。
harness-backup --config /etc/harness-server/backup.json verify --manifest <备份.json>

# 上传失败时保留原清单，补好凭据/网络后重试，不再生成一份新快照。
harness-backup --config /etc/harness-server/backup.json upload --manifest <备份.json>
```

上传前重新核对归档大小/SHA；依次上传 tar.gz 和对应 JSON，两个命令均成功退出后才登记 `.uploaded.json`。对象名为 `<prefix>/<UTC日期>/<备份文件名>`。重试使用相同对象名，`ossutil cp --force` 重新覆盖该备份的对象；上传新快照不会覆盖其他 UUID 的备份。归档上传成功但清单失败时仍整体报上传失败。工具不转发可能含敏感内容的 ossutil 原始输出，错误会指出本地清单以便重试。

上传结果只证明 ossutil 成功退出；远端下载SHA验证与恢复演练需单独做，不把退出成功解释为已完成恢复验证。没有自动删除本地或 OSS 历史备份；按磁盘容量维护本地副本，OSS 保留期可用 Bucket 生命周期配置。

## 每天自动备份

Linux 使用提供的 systemd unit。安装并确认 JSON 路径、数据卷权限、上传凭据可供 unit 的运行用户使用：

```sh
sudo install -m 644 deploy/harness-backup.service deploy/harness-backup.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl start harness-backup.service
sudo systemctl status harness-backup.service --no-pager
sudo systemctl enable --now harness-backup.timer
sudo systemctl list-timers harness-backup.timer --all
```

默认北京时间03:00，加0–5分钟随机延迟；Persistent=true会补跑停机期间错过的一次执行。需要其他时间时改timer，不逐轮备份。默认root用于读取Docker卷的私有目录；可改为具有数据读取及输出写入权限的专用用户。任务不会停止/启动Harness，也不会清空服务统计。

只有手动运行验证成功后再启用timer。配置 enabled=false 时定时任务只做本地备份，不会上传。查看失败：`journalctl -u harness-backup.service --since today`。2小时超时可能中断上传，原本地备份保留；按清单重试。并发手动/定时操作通过独立 `.backup.lock` 排他，不占用服务 `.writer.lock`。其他平台可自行构建备份工具；本版实测macOS和Linux，尚未实测Windows备份，也未发布预编译备份包。当前只提供Linux的定时安装模板。

## 一致性与范围

工具先读取原子HEAD并固定修订ID，再枚举已存在的修订。归档HEAD写入固定ID，后续服务提交新HEAD不会改变这份快照。归档包括该版本所有逻辑文档（含全部Session/Workstream、日志、任务/尝试/调度摘要）和完整幂等回执，以及扫描到的完整历史修订。清单记录当前文档数、回执数、历史修订数与SHA。

无 `.meta.json` 的未完成非HEAD修订排除并在清单列出；已存在但损坏/尚未写完整的清单导致本次备份失败，可稍后重试，不静默遗漏它。HEAD指向的当前修订必须完整且通过原生校验。备份不包含服务.env、客户端/OSS凭据、进程统计、writer锁或未被修订清单引用的文件。

在线模式依赖“修订一旦写完便不再修改”和“没有并发GC/恢复/迁移”。当前服务没有修订GC；若后续增加GC，必须增加备份固定修订的协调机制再启用清理。它不保证把备份期间新提交的内容也纳入本次固定HEAD；这些内容由下一份备份覆盖。

## 恢复演练

从OSS下载同名归档和JSON到配置的输出目录，先执行verify，再恢复到**独立空目录或新卷**，不要叠加已有数据：

```sh
umask 077
mkdir /srv/harness-restore-check
tar -xzf <备份.tar.gz> --no-same-owner -C /srv/harness-restore-check
harness-server --data-dir /srv/harness-restore-check serve --bind 127.0.0.1:18080
# 用原服务的凭据验证该临时服务：health、选定文档SHA、执行记录及幂等重试。
```

恢复仍需匹配存储格式的服务二进制；配置凭据单独注入，归档不携带它们。演练可以与线上并行。真正切换线上数据时停目标writer，保留当前数据，确认卷/UID权限后切换；服务启动的结构校验不是业务结论核验。verify仅检查归档校验和，不代替恢复演练。

## v0.3 namespace与管理目录兼容

本版工具识别并备份所有`namespaces/<id>/...`受管文档、`control/...`空间目录/授权/审计与持久回执；旧工具遇到不识别的路径会失败，不静默跳过。服务产生新路径前先升级备份工具，并做隔离恢复核验。页面保存的授权在归档中，部署身份绑定配置、Agent/管理员/OSS凭据及客户端配置不在归档中，需要单独管理。

## 从发布镜像获取备份工具

v0.3.0起服务镜像内置同版本`/usr/local/bin/harness-backup`。宿主机timer使用独立二进制时，可以从已拉取的、与本机架构一致的发布镜像复制；不在生产机编译、不改正在运行的服务。示例在Linux部署机执行：

```sh
docker create --name harness-backup-extract ghcr.io/ys1109/harness-server-public:0.3.0
docker cp harness-backup-extract:/usr/local/bin/harness-backup /tmp/harness-backup-v0.3.0
docker rm harness-backup-extract
chmod 755 /tmp/harness-backup-v0.3.0
/tmp/harness-backup-v0.3.0 --version
# 确认timer无正在运行的备份任务后，再安装宿主机二进制。
sudo install -m 755 /tmp/harness-backup-v0.3.0 /usr/local/bin/harness-backup
```

`docker create`仅创建用于复制文件的临时容器，不启动服务、挂载生产数据或执行构建。版本镜像不存在时先按镜像指南登录并pull；凭据不要写入命令参数或聊天。保留现有私有backup.json、timer及OSS配置，更新后先手动备份并隔离恢复核验。

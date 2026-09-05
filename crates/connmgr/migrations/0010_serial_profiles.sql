-- 串口档案参数（M7.4）。
--
-- 加列而不是并进 term_blob：串口参数是**连接参数**（决定怎么打开设备），
-- 与「终端长什么样」是两件事。塞进 term_blob 的话，一个只想改字号的用户
-- 会连带改写波特率所在的那段 JSON，而两者的生命周期完全不同。
--
-- 默认 '{}'：存量行零迁移平滑——`SerialSettings` 的每一栏都有 serde 默认值
--（115200 8N1、无流控），读出来就是一份合理的空档案。
ALTER TABLE profiles ADD COLUMN serial_blob TEXT NOT NULL DEFAULT '{}';

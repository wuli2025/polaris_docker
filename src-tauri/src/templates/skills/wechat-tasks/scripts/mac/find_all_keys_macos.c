/*
 * find_all_keys_macos.c — macOS 微信 4.x 数据库密钥内存扫描器
 * ─────────────────────────────────────────────────────────────
 * 原理：微信 macOS 版用 WCDB(SQLCipher 4) 加密本地库，每个 .db 文件有各自
 * 的 AES-256 密钥。WCDB 在打开库时把「已派生好的 raw key + salt」以可直接
 * 喂给 SQLCipher 的 PRAGMA 字符串形式缓存在进程内存里：
 *
 *        x'<64 位十六进制 key><32 位十六进制 salt>'
 *
 * 这里的 salt 恰好等于对应 .db 文件的前 16 字节。所以：
 *   1) 遍历数据目录，读每个 .db 的前 16 字节(salt)；
 *   2) 扫描微信进程可读可写内存，匹配上面的 ASCII 模式；
 *   3) 按 salt 把抓到的 key 对号入座到具体 db 文件；
 *   4) 写出 all_keys.json： { "message/message_0.db": {"enc_key": "hex"}, ... }
 *
 * 前置条件(macOS Hardened Runtime 默认禁止别的进程读它内存)：
 *   - 微信须 ad-hoc 重签名(去掉 Hardened Runtime)：
 *       codesign --force --deep --sign - /Applications/WeChat.app
 *     或整机关闭 SIP(不推荐，影响面大)；
 *   - 本程序须以 root 运行(sudo)，task_for_pid 才拿得到目标端口。
 *
 * 编译： cc -O2 -o find_all_keys_macos find_all_keys_macos.c
 * 运行： sudo ./find_all_keys_macos [pid]     # 省略 pid 则自动找微信
 *
 * 说明：纯 raw key（已派生），解密时无需再跑 PBKDF2 主密钥派生。
 * 参考公开做法(ydotdog/wechat-export-macos 等)，本文件为 Polaris 自研实现。
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <dirent.h>
#include <ftw.h>
#include <pwd.h>
#include <sys/stat.h>
#include <mach/mach.h>
#include <mach/mach_vm.h>

#define KEY_HEX_LEN 64          /* 32 字节 key = 64 hex */
#define SALT_HEX_LEN 32         /* 16 字节 salt = 32 hex */
#define PATTERN_HEX_LEN (KEY_HEX_LEN + SALT_HEX_LEN)   /* 96 */
#define MAX_KEYS 512
#define MAX_DBS 512
#define CHUNK_SIZE (4 * 1024 * 1024)

typedef struct {
    char key_hex[KEY_HEX_LEN + 1];
    char salt_hex[SALT_HEX_LEN + 1];
} key_entry_t;

static char g_db_salts[MAX_DBS][SALT_HEX_LEN + 1];
static char g_db_names[MAX_DBS][512];
static int  g_db_count = 0;

static int is_hex(unsigned char c) {
    return (c >= '0' && c <= '9') || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F');
}

/* 读 db 前 16 字节 salt 转 hex；明文库(以 "SQLite format 3" 开头)返回 -1 跳过 */
static int read_db_salt(const char *path, char *out) {
    FILE *f = fopen(path, "rb");
    if (!f) return -1;
    unsigned char h[16];
    size_t n = fread(h, 1, 16, f);
    fclose(f);
    if (n != 16) return -1;
    if (memcmp(h, "SQLite format 3", 15) == 0) return -1;
    for (int i = 0; i < 16; i++) sprintf(out + i * 2, "%02x", h[i]);
    out[SALT_HEX_LEN] = '\0';
    return 0;
}

static int nftw_collect(const char *fpath, const struct stat *sb, int flag, struct FTW *fb) {
    (void)sb; (void)fb;
    if (flag != FTW_F) return 0;
    size_t len = strlen(fpath);
    if (len < 3 || strcmp(fpath + len - 3, ".db") != 0) return 0;
    if (g_db_count >= MAX_DBS) return 0;
    char salt[SALT_HEX_LEN + 1];
    if (read_db_salt(fpath, salt) != 0) return 0;
    strcpy(g_db_salts[g_db_count], salt);
    /* 记录 db_storage/ 之后的相对路径，方便 Python 端对号入座 */
    const char *rel = strstr(fpath, "db_storage/");
    if (rel) rel += strlen("db_storage/");
    else { const char *s = strrchr(fpath, '/'); rel = s ? s + 1 : fpath; }
    strncpy(g_db_names[g_db_count], rel, 511);
    g_db_names[g_db_count][511] = '\0';
    g_db_count++;
    return 0;
}

static pid_t find_wechat_pid(void) {
    /* 4.x 进程名 WeChat；老壳/别名兜底再试 Weixin */
    const char *cands[] = {"pgrep -x WeChat", "pgrep -x Weixin", NULL};
    for (int i = 0; cands[i]; i++) {
        FILE *fp = popen(cands[i], "r");
        if (!fp) continue;
        char buf[64]; pid_t pid = -1;
        if (fgets(buf, sizeof(buf), fp)) pid = atoi(buf);
        pclose(fp);
        if (pid > 0) return pid;
    }
    return -1;
}

int main(int argc, char *argv[]) {
    pid_t pid = (argc >= 2) ? atoi(argv[1]) : find_wechat_pid();
    if (pid <= 0) { fprintf(stderr, "[!] 没找到运行中的微信进程\n"); return 1; }

    fprintf(stderr, "[*] 微信 PID=%d\n", pid);

    mach_port_t task;
    kern_return_t kr = task_for_pid(mach_task_self(), pid, &task);
    if (kr != KERN_SUCCESS) {
        fprintf(stderr, "[!] task_for_pid 失败(%d)。请确认：(1) 用 sudo 运行；"
                        "(2) 微信已 ad-hoc 重签名 或 SIP 已关。\n", kr);
        return 1;
    }

    /* 解析真实用户 HOME(sudo 下 HOME 可能变成 /var/root) */
    const char *home = getenv("HOME");
    const char *su = getenv("SUDO_USER");
    if (su) { struct passwd *pw = getpwnam(su); if (pw && pw->pw_dir) home = pw->pw_dir; }
    if (!home) home = getenv("HOME");

    /* 收集所有账号 db_storage 下的 db salt */
    char base[640];
    snprintf(base, sizeof(base),
        "%s/Library/Containers/com.tencent.xinWeChat/Data/Documents/xwechat_files", home);
    DIR *xdir = opendir(base);
    if (xdir) {
        struct dirent *e;
        while ((e = readdir(xdir))) {
            if (e->d_name[0] == '.') continue;
            char sp[1024];
            snprintf(sp, sizeof(sp), "%s/%s/db_storage", base, e->d_name);
            struct stat st;
            if (stat(sp, &st) == 0 && S_ISDIR(st.st_mode)) nftw(sp, nftw_collect, 24, FTW_PHYS);
        }
        closedir(xdir);
    }
    fprintf(stderr, "[*] 发现 %d 个加密 db\n", g_db_count);
    if (g_db_count == 0)
        fprintf(stderr, "[!] 没找到加密 db(可能数据目录不在默认位置，或库已是明文)。\n");

    /* 扫描内存 */
    key_entry_t keys[MAX_KEYS];
    int kc = 0;
    size_t scanned = 0;
    mach_vm_address_t addr = 0;
    while (1) {
        mach_vm_size_t size = 0;
        vm_region_basic_info_data_64_t info;
        mach_msg_type_number_t icount = VM_REGION_BASIC_INFO_COUNT_64;
        mach_port_t obj;
        kr = mach_vm_region(task, &addr, &size, VM_REGION_BASIC_INFO_64,
                            (vm_region_info_t)&info, &icount, &obj);
        if (kr != KERN_SUCCESS) break;
        if (size == 0) { addr++; continue; }

        int rw = (info.protection & (VM_PROT_READ | VM_PROT_WRITE)) == (VM_PROT_READ | VM_PROT_WRITE);
        if (rw) {
            mach_vm_address_t ca = addr;
            while (ca < addr + size) {
                mach_vm_size_t cs = addr + size - ca;
                if (cs > CHUNK_SIZE) cs = CHUNK_SIZE;
                vm_offset_t data; mach_msg_type_number_t dc;
                if (mach_vm_read(task, ca, cs, &data, &dc) == KERN_SUCCESS) {
                    unsigned char *b = (unsigned char *)data;
                    scanned += dc;
                    for (size_t i = 0; i + PATTERN_HEX_LEN + 3 < dc; i++) {
                        if (b[i] != 'x' || b[i + 1] != '\'') continue;
                        int ok = 1;
                        for (int j = 0; j < PATTERN_HEX_LEN; j++)
                            if (!is_hex(b[i + 2 + j])) { ok = 0; break; }
                        if (!ok || b[i + 2 + PATTERN_HEX_LEN] != '\'') continue;

                        char kh[KEY_HEX_LEN + 1], sh[SALT_HEX_LEN + 1];
                        memcpy(kh, b + i + 2, KEY_HEX_LEN); kh[KEY_HEX_LEN] = '\0';
                        memcpy(sh, b + i + 2 + KEY_HEX_LEN, SALT_HEX_LEN); sh[SALT_HEX_LEN] = '\0';
                        for (char *p = kh; *p; p++) if (*p >= 'A' && *p <= 'F') *p += 32;
                        for (char *p = sh; *p; p++) if (*p >= 'A' && *p <= 'F') *p += 32;

                        int dup = 0;
                        for (int k = 0; k < kc; k++)
                            if (!strcmp(keys[k].key_hex, kh) && !strcmp(keys[k].salt_hex, sh)) { dup = 1; break; }
                        if (dup) continue;
                        if (kc < MAX_KEYS) { strcpy(keys[kc].key_hex, kh); strcpy(keys[kc].salt_hex, sh); kc++; }
                    }
                    mach_vm_deallocate(mach_task_self(), data, dc);
                }
                /* 带重叠推进，避免跨块切断模式(x'..96..' = 99 字节) */
                ca += (cs > PATTERN_HEX_LEN + 3) ? cs - (PATTERN_HEX_LEN + 3) : cs;
            }
        }
        addr += size;
    }
    fprintf(stderr, "[*] 扫描 %zuMB，抓到 %d 个唯一 key\n", scanned / 1024 / 1024, kc);

    /* 按 salt 对号入座，写 all_keys.json 到 stdout(Python 端捕获重定向落盘) */
    printf("{\n");
    int first = 1, matched = 0;
    for (int i = 0; i < kc; i++) {
        const char *db = NULL;
        for (int j = 0; j < g_db_count; j++)
            if (!strcmp(keys[i].salt_hex, g_db_salts[j])) { db = g_db_names[j]; break; }
        if (!db) continue;
        printf("%s  \"%s\": {\"enc_key\": \"%s\", \"salt\": \"%s\"}",
               first ? "" : ",\n", db, keys[i].key_hex, keys[i].salt_hex);
        first = 0; matched++;
    }
    printf("\n}\n");
    fprintf(stderr, "[*] 对号入座 %d/%d 个 key\n", matched, kc);
    if (matched == 0) {
        fprintf(stderr, "[!] 一个都没对上——多半是微信还没解锁登录到主界面(key 尚未进内存)。\n");
        return 3;
    }
    return 0;
}

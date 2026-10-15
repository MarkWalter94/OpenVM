#include "json.h"

static size_t length(const char *s) { size_t n = 0; while (s[n]) ++n; return n; }
static void whitespace(JDoc *d) {
    while (d->position < d->length) {
        char c = d->text[d->position];
        if (c != ' ' && c != '\n' && c != '\r' && c != '\t') break;
        ++d->position;
    }
}
static int hex(char c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}
static int string_token(JDoc *d) {
    if (d->text[d->position++] != '"') return 0;
    while (d->position < d->length) {
        unsigned char c = (unsigned char)d->text[d->position++];
        if (c == '"') return 1;
        if (c < 32) return 0;
        if (c == '\\') {
            if (d->position == d->length) return 0;
            c = (unsigned char)d->text[d->position++];
            if (c == 'u') {
                unsigned i;
                for (i = 0; i < 4; ++i) {
                    if (d->position == d->length || hex(d->text[d->position++]) < 0) return 0;
                }
            } else if (c != '"' && c != '\\' && c != '/' && c != 'b' && c != 'f' && c != 'n' && c != 'r' && c != 't') return 0;
        }
    }
    return 0;
}
static int value(JDoc *d, int parent, unsigned depth) {
    size_t i, start; int index, kind; char c;
    whitespace(d);
    if (depth > 64 || d->position == d->length || d->count == d->capacity) return -1;
    index = (int)d->count++;
    start = d->position;
    c = d->text[d->position];
    d->tokens[index].parent = parent;
    d->tokens[index].start = start;
    if (c == '{' || c == '[') {
        char end = c == '{' ? '}' : ']';
        kind = c == '{' ? J_OBJECT : J_ARRAY;
        ++d->position;
        whitespace(d);
        if (d->position < d->length && d->text[d->position] == end) ++d->position;
        else for (;;) {
            if (kind == J_OBJECT) {
                if (d->position == d->length || d->text[d->position] != '"') return -1;
                if (value(d, index, depth + 1) < 0) return -1;
                whitespace(d);
                if (d->position == d->length || d->text[d->position++] != ':') return -1;
            }
            if (value(d, index, depth + 1) < 0) return -1;
            whitespace(d);
            if (d->position == d->length) return -1;
            c = d->text[d->position++];
            if (c == end) break;
            if (c != ',') return -1;
            whitespace(d);
        }
    } else if (c == '"') {
        kind = J_STRING;
        if (!string_token(d)) return -1;
    } else if (c == '-' || (c >= '0' && c <= '9')) {
        kind = J_NUMBER;
        if (c == '-') ++d->position;
        if (d->position == d->length) return -1;
        c = d->text[d->position];
        if (c == '0') ++d->position;
        else {
            if (c < '1' || c > '9') return -1;
            do { ++d->position; } while (d->position < d->length && d->text[d->position] >= '0' && d->text[d->position] <= '9');
        }
        /* The Tools protocol contains only integers; no NaN, exponents or fractions. */
    } else {
        const char *word;
        if (c == 't') { word = "true"; kind = J_TRUE; }
        else if (c == 'f') { word = "false"; kind = J_FALSE; }
        else if (c == 'n') { word = "null"; kind = J_NULL; }
        else return -1;
        for (i = 0; word[i]; ++i) {
            if (d->position == d->length || d->text[d->position++] != word[i]) return -1;
        }
    }
    d->tokens[index].kind = kind;
    d->tokens[index].end = d->position;
    return index;
}
int jparse(JDoc *d, const char *data, size_t size, JToken *tokens, size_t capacity) {
    d->text = data; d->length = size; d->position = 0;
    d->tokens = tokens; d->count = 0; d->capacity = capacity;
    if (!size || value(d, -1, 0) != 0) return 0;
    whitespace(d);
    return d->position == d->length;
}
int jget(const JDoc *d, int object, const char *key) {
    size_t i;
    if (object < 0 || (size_t)object >= d->count || d->tokens[object].kind != J_OBJECT) return -1;
    for (i = (size_t)object + 1; i < d->count; ++i) {
        if (d->tokens[i].start >= d->tokens[object].end) break;
        if (d->tokens[i].parent == object && d->tokens[i].kind == J_STRING) {
            int val = (int)i + 1;
            if (jequal(d, (int)i, key)) return val;
            /* Skip the value subtree, so a string value is not interpreted as a key. */
            if ((size_t)val >= d->count) return -1;
            while (i + 1 < d->count && d->tokens[i + 1].start < d->tokens[val].end) ++i;
        }
    }
    return -1;
}
static int append_utf8(char *out, size_t *n, size_t cap, uint32_t cp) {
    unsigned char bytes[4]; size_t count, i;
    if (cp < 0x80) { bytes[0] = (unsigned char)cp; count = 1; }
    else if (cp < 0x800) { bytes[0] = 0xc0 | (cp >> 6); bytes[1] = 0x80 | (cp & 63); count = 2; }
    else if (cp < 0x10000) { bytes[0] = 0xe0 | (cp >> 12); bytes[1] = 0x80 | ((cp >> 6) & 63); bytes[2] = 0x80 | (cp & 63); count = 3; }
    else { bytes[0] = 0xf0 | (cp >> 18); bytes[1] = 0x80 | ((cp >> 12) & 63); bytes[2] = 0x80 | ((cp >> 6) & 63); bytes[3] = 0x80 | (cp & 63); count = 4; }
    if (*n + count >= cap || cp == 0) return 0;
    for (i = 0; i < count; ++i) out[(*n)++] = (char)bytes[i];
    return 1;
}
int jstring(const JDoc *d, int token, char *out, size_t cap) {
    size_t p, end, n = 0;
    if (token < 0 || (size_t)token >= d->count || d->tokens[token].kind != J_STRING || !cap) return 0;
    p = d->tokens[token].start + 1; end = d->tokens[token].end - 1;
    while (p < end) {
        unsigned char c = (unsigned char)d->text[p++];
        if (c == '\\') {
            c = (unsigned char)d->text[p++];
            if (c == 'u') {
                uint32_t cp = 0; unsigned i;
                for (i = 0; i < 4; ++i) cp = (cp << 4) | (unsigned)hex(d->text[p++]);
                if (cp >= 0xd800 && cp <= 0xdbff) {
                    uint32_t low = 0;
                    if (end - p < 6 || d->text[p++] != '\\' || d->text[p++] != 'u') return 0;
                    for (i = 0; i < 4; ++i) low = (low << 4) | (unsigned)hex(d->text[p++]);
                    if (low < 0xdc00 || low > 0xdfff) return 0;
                    cp = 0x10000 + ((cp - 0xd800) << 10) + low - 0xdc00;
                } else if (cp >= 0xdc00 && cp <= 0xdfff) return 0;
                if (!append_utf8(out, &n, cap, cp)) return 0;
                continue;
            }
            if (c == 'b') c = '\b'; else if (c == 'f') c = '\f';
            else if (c == 'n') c = '\n'; else if (c == 'r') c = '\r'; else if (c == 't') c = '\t';
        }
        if (n + 1 >= cap || !c) return 0;
        out[n++] = (char)c;
    }
    out[n] = 0;
    return 1;
}
int jequal(const JDoc *d, int token, const char *value) {
    char decoded[128]; size_t i;
    if (!jstring(d, token, decoded, sizeof(decoded))) return 0;
    for (i = 0; decoded[i] && value[i] && decoded[i] == value[i]; ++i) {}
    return decoded[i] == 0 && value[i] == 0;
}
uint64_t jnumber(const JDoc *d, int token, uint64_t fallback) {
    size_t i; uint64_t n = 0;
    if (token < 0 || (size_t)token >= d->count || d->tokens[token].kind != J_NUMBER) return fallback;
    for (i = d->tokens[token].start; i < d->tokens[token].end; ++i) {
        unsigned c = (unsigned char)d->text[i];
        if (c < '0' || c > '9' || n > (UINT64_MAX - (c - '0')) / 10) return fallback;
        n = n * 10 + c - '0';
    }
    return n;
}
void jout(JOut *o, char *data, size_t cap) { o->data = data; o->length = 0; o->capacity = cap; o->ok = 1; }
void jraw(JOut *o, const char *data, size_t size) {
    size_t i;
    if (!o->ok || size > o->capacity - o->length) { o->ok = 0; return; }
    for (i = 0; i < size; ++i) o->data[o->length++] = data[i];
}
void jtext(JOut *o, const char *s) { jraw(o, s, length(s)); }
void jquote(JOut *o, const char *s) {
    static const char h[] = "0123456789abcdef";
    jtext(o, "\"");
    for (; *s; ++s) {
        unsigned char c = (unsigned char)*s;
        if (c == '"' || c == '\\') { jtext(o, "\\"); jraw(o, s, 1); }
        else if (c < 32) { char escape[6] = {'\\','u','0','0',h[c >> 4],h[c & 15]}; jraw(o, escape, 6); }
        else jraw(o, s, 1);
    }
    jtext(o, "\"");
}
void juint(JOut *o, uint64_t n) {
    char digits[32]; size_t i = 0;
    do { digits[i++] = (char)('0' + n % 10); n /= 10; } while (n);
    while (i) jraw(o, &digits[--i], 1);
}
void jint(JOut *o, int32_t value) {
    if (value < 0) { jtext(o, "-"); juint(o, (uint64_t)(-(int64_t)value)); }
    else juint(o, (uint32_t)value);
}

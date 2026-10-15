/* LightVM guest Tools JSON codec. MIT. No CRT dependency. */
#ifndef LIGHTVM_JSON_H
#define LIGHTVM_JSON_H
#include <stddef.h>
#include <stdint.h>

enum { J_OBJECT, J_ARRAY, J_STRING, J_NUMBER, J_TRUE, J_FALSE, J_NULL };
typedef struct { int kind, parent; size_t start, end; } JToken;
typedef struct {
    const char *text; size_t length, position;
    JToken *tokens; size_t count, capacity;
} JDoc;
typedef struct { char *data; size_t length, capacity; int ok; } JOut;

int jparse(JDoc *doc, const char *data, size_t length, JToken *tokens, size_t capacity);
int jget(const JDoc *doc, int object, const char *key);
int jstring(const JDoc *doc, int token, char *output, size_t capacity);
uint64_t jnumber(const JDoc *doc, int token, uint64_t fallback);
int jequal(const JDoc *doc, int token, const char *value);
void jout(JOut *out, char *data, size_t capacity);
void jraw(JOut *out, const char *data, size_t length);
void jtext(JOut *out, const char *data);
void jquote(JOut *out, const char *data);
void juint(JOut *out, uint64_t value);
void jint(JOut *out, int32_t value);
#endif

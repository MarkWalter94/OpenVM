#include "json.h"
#include <assert.h>
#include <string.h>
#include <stdlib.h>
int main(void) {
    JDoc doc; JToken tokens[256]; char output[128]; JOut out;
    const char *good="{\"op\":\"clipboard_write\",\"text\":\"quote \\\" and \\ud83d\\ude00\",\"n\":18446744073709551615,\"nested\":{\"op\":\"wrong\"}}";
    assert(jparse(&doc,good,strlen(good),tokens,256));
    assert(jequal(&doc,jget(&doc,0,"op"),"clipboard_write"));
    assert(jnumber(&doc,jget(&doc,0,"n"),0)==UINT64_MAX);
    assert(jstring(&doc,jget(&doc,0,"text"),output,sizeof(output)));
    assert(!strcmp(output,"quote \" and \xf0\x9f\x98\x80"));
    jout(&out,output,sizeof(output));jquote(&out,"line\nquote\"");
    assert(out.ok&&jparse(&doc,output,out.length,tokens,256));
    const char *bad[]={"", "{", "[1,]", "{\"x\":}", "{}{}", "01", "1.2", "\"bad\n\"", "\"\\uZZZZ\"", "[true false]"};
    for(size_t i=0;i<sizeof(bad)/sizeof(bad[0]);++i)assert(!jparse(&doc,bad[i],strlen(bad[i]),tokens,256));
    char bytes[1024];
    for(unsigned n=0;n<50000;++n){
        size_t size=rand()%sizeof(bytes);
        for(size_t i=0;i<size;++i)bytes[i]=(char)(rand()%256);
        if(jparse(&doc,bytes,size,tokens,256)){
            for(size_t i=0;i<doc.count;++i){jstring(&doc,(int)i,output,sizeof(output));jnumber(&doc,(int)i,0);jget(&doc,(int)i,"op");}
        }
    }
    return 0;
}

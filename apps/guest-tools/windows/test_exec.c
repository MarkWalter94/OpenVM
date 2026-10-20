/* Native lifecycle tests; compile with json.c and run on a Windows host. */
#define WIN32_LEAN_AND_MEAN
#define _WIN32_WINNT 0x0501
#include <windows.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <assert.h>
#include "json.h"
static int wide(const char *s,WCHAR *out,int capacity){return MultiByteToWideChar(CP_UTF8,MB_ERR_INVALID_CHARS,s,-1,out,capacity)>0;}
static size_t wlength(const WCHAR *s){size_t n=0;while(s[n])++n;return n;}
static int wappend(WCHAR *dst,size_t capacity,const WCHAR *src){size_t n=wlength(dst),m=wlength(src);if(n+m>=capacity)return 0;memcpy(dst+n,src,(m+1)*sizeof(WCHAR));return 1;}
static void error_reply(JOut *out,const char *code){jtext(out,"{\"result\":\"error\",\"code\":");jquote(out,code);jtext(out,"}");}
static DWORD simulated_version;
static DWORD exec_test_version(void){return simulated_version?simulated_version:GetVersion();}
#define GetVersion exec_test_version
#include "exec.h"
#undef GetVersion

static void start(const char *command,DWORD timeout,uint64_t id){
    char request[16000],reply[32000];JOut out;JDoc doc;JToken tokens[32];
    jout(&out,request,sizeof(request));jtext(&out,"{\"command\":");jquote(&out,command);jtext(&out,",\"timeout_ms\":");juint(&out,timeout);jtext(&out,"}");
    assert(out.ok);assert(jparse(&doc,request,out.length,tokens,32));
    jout(&out,reply,sizeof(reply));guest_exec(&out,&doc,0,id,1,0);assert(out.ok);
    if(!execution.worker){fwrite(reply,1,out.length,stderr);assert(execution.worker);}
}
static void finish(void){assert(WaitForSingleObject(execution.worker,5000)==WAIT_OBJECT_0);assert(!execution.active);}
int main(void){
    InitializeCriticalSection(&execution.lock);
    start("echo hello & echo problem 1>&2 & exit /b 7",3000,1);finish();
    assert(execution.code==7);assert(strstr(execution.stdout_data,"hello"));assert(strstr(execution.stderr_data,"problem"));
    exec_clear();
    start("for /L %i in (1,1,2147483647) do @rem",50,2);finish();assert(execution.timed_out);
    exec_clear();
    start("for /L %i in (1,1,2147483647) do @rem",30000,3);
    {
        char request[]="{\"job\":3}",reply[32000];JDoc doc;JToken tokens[8];JOut out;
        assert(jparse(&doc,request,strlen(request),tokens,8));jout(&out,reply,sizeof(reply));guest_exec(&out,&doc,0,4,0,1);
    }
    finish();assert(execution.cancelled);exec_clear();
    start("for /L %i in (1,1,3000) do @echo abcdefghijklmnopqrstuvwxyz",3000,4);finish();
    assert(execution.code==0);assert(execution.truncated);assert(execution.stdout_size==EXEC_OUTPUT);exec_clear();
    start("for /L %i in (1,1,2147483647) do @rem",30000,5);exec_clear();assert(!execution.active);
    {
        HANDLE parent=CreateJobObjectW(NULL,NULL);BOOL member;
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits;
        memset(&limits,0,sizeof(limits));
        assert(parent&&SetInformationJobObject(parent,JobObjectExtendedLimitInformation,&limits,sizeof(limits)));
        assert(AssignProcessToJobObject(parent,GetCurrentProcess()));
        /* Modern Windows must work when the parent forbids breakaway. */
        start("for /L %i in (1,1,2147483647) do @rem",50,6);
        assert(IsProcessInJob(execution.process,parent,&member)&&member);
        finish();assert(execution.timed_out);exec_clear();
        limits.BasicLimitInformation.LimitFlags=JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        assert(SetInformationJobObject(parent,JobObjectExtendedLimitInformation,&limits,sizeof(limits)));
        simulated_version=0x00000106; /* Windows 7: major 6, minor 1. */
        start("for /L %i in (1,1,2147483647) do @rem",50,7);
        assert(IsProcessInJob(execution.process,parent,&member)&&!member);
        finish();assert(execution.timed_out);exec_clear();CloseHandle(parent);
    }
    DeleteCriticalSection(&execution.lock);puts("Guest execution lifecycle: PASS");return 0;
}

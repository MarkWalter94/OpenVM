/* Disposable Windows PE guest probe. Never run against the host TPM. */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
static void text(const char *s) { DWORD n=0; const char *e=s; while(*e)++e; WriteFile(GetStdHandle(STD_OUTPUT_HANDLE),s,(DWORD)(e-s),&n,NULL); }
static void number(DWORD value) { char s[11]; unsigned i=0; do { s[i++]=(char)('0'+value%10); value/=10; } while(value); while(i){char c=s[--i];DWORD n;WriteFile(GetStdHandle(STD_OUTPUT_HANDLE),&c,1,&n,NULL);} }
static DWORD be32(const BYTE *p){return ((DWORD)p[0]<<24)|((DWORD)p[1]<<16)|((DWORD)p[2]<<8)|p[3];}
typedef DWORD (WINAPI *CreateContext)(const void*,void**);
typedef DWORD (WINAPI *CloseContext)(void*);
typedef DWORD (WINAPI *Submit)(void*,DWORD,DWORD,const BYTE*,DWORD,BYTE*,DWORD*);
static int persistent_nv(HMODULE library, int mutate) {
    CreateContext create=(CreateContext)(void*)GetProcAddress(library,"Tbsi_Context_Create");
    CloseContext close=(CloseContext)(void*)GetProcAddress(library,"Tbsip_Context_Close");
    Submit submit=(Submit)(void*)GetProcAddress(library,"Tbsip_Submit_Command");
    DWORD params[2]={2,4},status,size=1024;void *context=NULL;BYTE reply[1024];unsigned i;
    /* Owner-authenticated test-only NV index 0x01500020, empty auth.
       TPMA_NV_OWNERWRITE|OWNERREAD|NO_DA. All commands use a password session. */
    static const BYTE read[]={0x80,2,0,0,0,35,0,0,1,0x4e,0x40,0,0,1,1,0x50,0,0x20,0,0,0,9,0x40,0,0,9,0,0,0,0,0,0,8,0,0};
    static const BYTE define[]={0x80,2,0,0,0,45,0,0,1,0x2a,0x40,0,0,1,0,0,0,9,0x40,0,0,9,0,0,0,0,0,0,0,0,14,1,0x50,0,0x20,0,0x0b,2,2,0,2,0,0,0,8};
    BYTE write[]={0x80,2,0,0,0,43,0,0,1,0x37,0x40,0,0,1,1,0x50,0,0x20,0,0,0,9,0x40,0,0,9,0,0,0,0,0,0,8,'L','i','g','h','t','V','M','2',0,0};
    static const BYTE expected[]={'L','i','g','h','t','V','M','2'};
    if(!create||!close||!submit)return -1;
    status=create(params,&context);if(status){text("context_status=");number(status);text("\r\n");return -1;}
    status=submit(context,0,200,read,sizeof(read),reply,&size);
    if(!status&&size>=24&&!be32(reply+6)&&reply[14]==0&&reply[15]==8){
        for(i=0;i<8;++i)if(reply[16+i]!=expected[i]){close(context);return -1;}
        if(mutate){
            write[40]='3';size=sizeof(reply);status=submit(context,0,200,write,sizeof(write),reply,&size);
            close(context);return (!status&&size>=10&&!be32(reply+6))?2:-1;
        }
        close(context);return 1;
    }
    text("initial_nv_status=");number(status?status:(size>=10?be32(reply+6):0xffffffff));text("\r\n");
    size=sizeof(reply);status=submit(context,0,200,define,sizeof(define),reply,&size);
    if(status||size<10||be32(reply+6)){text("nv_define_status=");number(status?status:(size>=10?be32(reply+6):0xffffffff));text("\r\n");close(context);return -1;}
    size=sizeof(reply);status=submit(context,0,200,write,sizeof(write),reply,&size);
    close(context);if(status||size<10||be32(reply+6))return -1;return 0;
}
void mainCRTStartup(void) {
    HANDLE token; TOKEN_PRIVILEGES privilege; BYTE secure=0,setup=255; DWORD count;
    typedef DWORD (WINAPI *DeviceInfo)(UINT32,PVOID);
    HMODULE library; DeviceInfo info; DWORD device[4]={1,0,0,0},status=0xffffffff;int nv=-1;
    if(OpenProcessToken(GetCurrentProcess(),TOKEN_ADJUST_PRIVILEGES|TOKEN_QUERY,&token)) {
        privilege.PrivilegeCount=1; privilege.Privileges[0].Attributes=SE_PRIVILEGE_ENABLED;
        if(LookupPrivilegeValueW(NULL,L"SeSystemEnvironmentPrivilege",&privilege.Privileges[0].Luid)) AdjustTokenPrivileges(token,FALSE,&privilege,0,NULL,NULL);
        CloseHandle(token);
    }
    count=GetFirmwareEnvironmentVariableW(L"SecureBoot",L"{8BE4DF61-93CA-11D2-AA0D-00E098032B8C}",&secure,1);
    text("secure_boot=");number(count==1?secure:255);text("\r\n");
    count=GetFirmwareEnvironmentVariableW(L"SetupMode",L"{8BE4DF61-93CA-11D2-AA0D-00E098032B8C}",&setup,1);
    text("setup_mode=");number(count==1?setup:255);text("\r\n");
    library=LoadLibraryExW(L"tbs.dll",NULL,LOAD_LIBRARY_SEARCH_SYSTEM32);
    info=library?(DeviceInfo)(void*)GetProcAddress(library,"Tbsi_GetDeviceInfo"):NULL;
    if(info)status=info(sizeof(device),device);
    text("tbs_status=");number(status);text("\r\ntpm_version=");number(device[1]);text("\r\n");
    if(!status&&library){
        const WCHAR *command=GetCommandLineW();int mutate=0;
        while(*command){if(command[0]=='-'&&command[1]=='-'&&command[2]=='m'){mutate=1;break;}++command;}
        nv=persistent_nv(library,mutate);
    }
    text(nv==2?"persistent_nv=mutated\r\n":nv==1?"persistent_nv=matched\r\n":nv==0?"persistent_nv=created\r\n":"persistent_nv=failed\r\n");
    if(library)FreeLibrary(library);
    ExitProcess(secure==1&&setup==0&&status==0&&device[1]==2&&nv>=0?0:1);
}

using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json;
using System.Threading.Tasks;
using Dahomey.Json;
using IdentityServer4.Stores;
using Karenia.Rurikawa.Coordinator.Services;
using Karenia.Rurikawa.Helpers;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Hosting;
using Microsoft.AspNetCore.HttpsPolicy;
using Microsoft.AspNetCore.Mvc;
using Microsoft.EntityFrameworkCore;
using Microsoft.Extensions.Configuration;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Hosting;
using Microsoft.Extensions.Logging;

namespace Karenia.Rurikawa.Coordinator {
    public class Startup {
        public Startup(IConfiguration configuration) {
            Configuration = configuration;
        }

        public IConfiguration Configuration { get; }

        // This method gets called by the runtime. Use this method to add services to the container.
        public void ConfigureServices(IServiceCollection services) {
            services.AddLogging();

            // TODO: Change to real identity stuff
            services.AddIdentityServer()
                .AddInMemoryClients(Config.GetClients())
                .AddInMemoryApiResources(Config.GetApiResources())
                .AddResourceOwnerValidator<IdentityService>();
            services.AddTransient<IRefreshTokenStore, RefreshTokenStore>();

            var pgsqlLinkParams = Configuration.GetValue<string>("pgsqlLink");
            var testStorageParams = new SingleBucketFileStorageService.Params();
            Configuration.GetSection("testStorage").Bind(testStorageParams);

            services.AddDbContextPool<Models.RurikawaDb>(options => {
                options.UseNpgsql(pgsqlLinkParams);
            });
            services.AddSingleton(
                svc => new SingleBucketFileStorageService(
                    testStorageParams,
                    svc.GetService<ILogger<SingleBucketFileStorageService>>())
            );
            services.AddSingleton<JudgerCoordinatorService>();
            services.AddScoped<AccountService>();
            services.AddScoped<IdentityService>();
            services.AddSingleton<JsonSerializerOptions>(_ =>
                SetupJsonSerializerOptions(new JsonSerializerOptions())
            );
            services.AddRouting(options => { options.LowercaseUrls = true; });
            services.AddControllers().AddJsonOptions(opt => SetupJsonSerializerOptions(opt.JsonSerializerOptions));
        }

        public JsonSerializerOptions SetupJsonSerializerOptions(JsonSerializerOptions opt) {
            opt.PropertyNamingPolicy = JsonNamingPolicy.CamelCase;
            opt.Converters.Add(new FlowSnakeJsonConverter());
            opt.SetupExtensions();
            return opt;
        }

        // This method gets called by the runtime. Use this method to configure the HTTP request pipeline.
        public void Configure(IApplicationBuilder app, IWebHostEnvironment env) {
            if (env.IsDevelopment()) {
                app.UseDeveloperExceptionPage();
            }

            if (!env.IsDevelopment()) { app.UseHttpsRedirection(); }

            app.UseRouting();
            // TODO: Add websocket options
            app.UseWebSockets();
            app.UseAuthorization();

            // Add websocket acceptor
            app.Use(async (ctx, next) => {
                if (ctx.WebSockets.IsWebSocketRequest) {
                    var svc = app.ApplicationServices.GetService<JudgerCoordinatorService>();
                    await svc.TryUseConnection(ctx);
                }
                await next();
            });

            app.UseEndpoints(endpoints => {
                endpoints.MapControllers();
            });
        }
    }
}
